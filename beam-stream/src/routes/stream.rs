use crate::state::AppState;
use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::Serialize;
use std::path::PathBuf;
use tokio::fs::File;
use tracing::{debug, error, trace};

#[derive(Serialize, ToSchema)]
pub struct StreamTokenResponse {
    pub token: String,
}

#[derive(Debug, PartialEq)]
pub(crate) enum RangeError {
    MissingBytesPrefix,
    MalformedRange,
    NonNumericBound,
    RangeNotSatisfiable { start: u64, file_size: u64 },
}

/// Parse an HTTP Range header value against a known file size.
///
/// Returns `Ok((start, end))` where both are inclusive byte offsets,
/// or a `RangeError` describing the failure mode.
pub(crate) fn parse_byte_range(
    header_value: &str,
    file_size: u64,
) -> Result<(u64, u64), RangeError> {
    if file_size == 0 {
        return Err(RangeError::RangeNotSatisfiable {
            start: 0,
            file_size: 0,
        });
    }

    if !header_value.starts_with("bytes=") {
        return Err(RangeError::MissingBytesPrefix);
    }

    let range_part = &header_value[6..]; // strip "bytes="
    let dash_pos = range_part.find('-').ok_or(RangeError::MalformedRange)?;
    let start_str = &range_part[..dash_pos];
    let end_str = &range_part[dash_pos + 1..];

    if start_str.is_empty() && end_str.is_empty() {
        return Err(RangeError::MalformedRange);
    }

    let (start, end) = if start_str.is_empty() {
        // Suffix range: "bytes=-N" means the last N bytes
        let suffix = end_str
            .parse::<u64>()
            .map_err(|_| RangeError::NonNumericBound)?;
        let start = file_size.saturating_sub(suffix);
        (start, file_size - 1)
    } else {
        let start = start_str
            .parse::<u64>()
            .map_err(|_| RangeError::NonNumericBound)?;
        let end = if end_str.is_empty() {
            // Open-ended range: "bytes=N-"
            file_size - 1
        } else {
            let e = end_str
                .parse::<u64>()
                .map_err(|_| RangeError::NonNumericBound)?;
            std::cmp::min(e, file_size - 1)
        };
        (start, end)
    };

    if start > end || start >= file_size {
        return Err(RangeError::RangeNotSatisfiable { start, file_size });
    }

    Ok((start, end))
}

/// Get a presigned token for streaming
#[endpoint(
    tags("stream"),
    parameters(
        ("id" = String, description = "Stream ID")
    ),
    responses(
        (status_code = 200, description = "Stream token"),
        (status_code = 401, description = "Unauthorized"),
        (status_code = 404, description = "Stream not found")
    )
)]
pub async fn get_stream_token(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let state = depot.obtain::<AppState>().unwrap();
    let id: String = req.param::<String>("id").unwrap_or_default();

    // Validate user auth
    let user_id = if let Some(auth_header) = req.headers().get("Authorization")
        && let Ok(auth_str) = auth_header.to_str()
        && auth_str.starts_with("Bearer ")
    {
        let token = &auth_str[7..];
        match state.services.auth.verify_token(token).await {
            Ok(user) => user.user_id,
            Err(_) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                return;
            }
        }
    } else {
        res.status_code(StatusCode::UNAUTHORIZED);
        return;
    };

    // Verify the file exists before issuing a token
    match state.services.library.get_file_by_id(id.clone()).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            res.status_code(StatusCode::NOT_FOUND);
            return;
        }
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    }

    // Create stream token
    match state.services.auth.create_stream_token(&user_id, &id) {
        Ok(token) => {
            res.render(Json(StreamTokenResponse { token }));
        }
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        }
    }
}

/// Stream via MP4 - serves AVFoundation-friendly fragmented MP4
#[endpoint(
    tags("media"),
    parameters(
        ("id" = String, description = "Stream ID"),
        ("token" = String, description = "Presigned stream token")
    ),
    responses(
        (status_code = 200, description = "Media stream"),
        (status_code = 401, description = "Invalid or expired token"),
        (status_code = 404, description = "File not found"),
        (status_code = 416, description = "Range not satisfiable"),
        (status_code = 500, description = "Internal server error")
    )
)]
#[tracing::instrument(skip_all)]
pub async fn stream_mp4(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let state = depot.obtain::<AppState>().unwrap();
    let id: String = req.param::<String>("id").unwrap_or_default();
    let token: String = req.query::<String>("token").unwrap_or_default();

    // Validate stream token
    match state.services.auth.verify_stream_token(&token) {
        Ok(stream_id) => {
            if stream_id != id {
                res.status_code(StatusCode::UNAUTHORIZED);
                return;
            }
        }
        Err(_) => {
            res.status_code(StatusCode::UNAUTHORIZED);
            return;
        }
    }

    debug!("Streaming media with ID: {}", id);

    // Look up the file by ID to get its actual path
    let file = match state.services.library.get_file_by_id(id.clone()).await {
        Ok(Some(f)) => f,
        Ok(None) => {
            res.status_code(StatusCode::NOT_FOUND);
            return;
        }
        Err(_) => {
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let source_video_path = PathBuf::from(&file.path);
    let cache_mp4_path = state.config.cache_dir.join(format!("{}.mp4", id));

    if !source_video_path.exists() {
        error!("Source video file not found: {:?}", source_video_path);
        res.status_code(StatusCode::NOT_FOUND);
        return;
    }

    // Generate MP4 if it doesn't exist or is outdated
    if !cache_mp4_path.exists() {
        trace!("Cached MP4 not found, generating: {:?}", cache_mp4_path);

        if let Err(err) = state
            .services
            .transcode
            .generate_mp4_cache(&source_video_path, &cache_mp4_path)
            .await
        {
            error!("Failed to generate MP4: {:?}", err);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }

        trace!("MP4 generation complete: {:?}", cache_mp4_path);
    } else {
        trace!("Using cached MP4: {:?}", cache_mp4_path);
    }

    // Serve the MP4 file with range request support
    serve_mp4_file(&cache_mp4_path, req, res).await;
}

/// Serve MP4 file with HTTP range request support for AVFoundation
async fn serve_mp4_file(file_path: &PathBuf, req: &Request, res: &mut Response) {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    // Get file metadata
    let file_metadata = match tokio::fs::metadata(file_path).await {
        Ok(metadata) => metadata,
        Err(err) => {
            error!("Failed to get file metadata: {:?}", err);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    let file_size = file_metadata.len();

    // Always use video/mp4 content type since we're serving MP4
    let content_type = "video/mp4";

    // Handle range requests
    let range = req.headers().get("range");
    let (start, end, status_code) = if let Some(range_header) = range {
        let range_str = match range_header.to_str() {
            Ok(s) => s,
            Err(_) => {
                res.status_code(StatusCode::BAD_REQUEST);
                return;
            }
        };

        match parse_byte_range(range_str, file_size) {
            Ok((start, end)) => (start, end, StatusCode::PARTIAL_CONTENT),
            Err(RangeError::RangeNotSatisfiable { .. }) => {
                res.status_code(StatusCode::RANGE_NOT_SATISFIABLE);
                return;
            }
            Err(_) => {
                res.status_code(StatusCode::BAD_REQUEST);
                return;
            }
        }
    } else {
        (0, file_size - 1, StatusCode::OK)
    };

    // Open file and seek to start position
    let mut file = match File::open(file_path).await {
        Ok(f) => f,
        Err(err) => {
            error!("Failed to open file: {:?}", err);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    };

    // Seek to start position if needed
    if start > 0
        && let Err(err) = file.seek(std::io::SeekFrom::Start(start)).await
    {
        error!("Failed to seek in file: {:?}", err);
        res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
        return;
    }

    let content_length = end - start + 1;

    // Build response
    res.status_code(status_code);
    res.headers_mut()
        .insert("Content-Type", content_type.parse().unwrap());
    res.headers_mut().insert(
        "Content-Length",
        content_length.to_string().parse().unwrap(),
    );
    res.headers_mut()
        .insert("Accept-Ranges", "bytes".parse().unwrap());

    // Add range headers for partial content
    if status_code == StatusCode::PARTIAL_CONTENT {
        res.headers_mut().insert(
            "Content-Range",
            format!("bytes {}-{}/{}", start, end, file_size)
                .parse()
                .unwrap(),
        );
    }

    // Add cache headers for better performance
    res.headers_mut()
        .insert("Cache-Control", "public, max-age=3600".parse().unwrap());
    res.headers_mut()
        .insert("ETag", format!("\"{}\"", file_size).parse().unwrap()); // Simple ETag based on file size

    // Stream the range lazily in chunks to avoid buffering the entire range in memory.
    let chunk_size = 128 * 1024usize;
    let stream = async_stream::stream! {
        let mut remaining = content_length as usize;
        while remaining > 0 {
            let to_read = chunk_size.min(remaining);
            let mut buf = vec![0u8; to_read];
            match file.read_exact(&mut buf).await {
                Ok(_) => {
                    remaining -= to_read;
                    yield Ok::<_, std::io::Error>(bytes::Bytes::from(buf));
                }
                Err(e) => {
                    yield Err(e);
                    break;
                }
            }
        }
    };
    res.body(salvo::http::body::ResBody::stream(stream));
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod stream_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use salvo::test::ResponseExt;

    /// Verify that `serve_mp4_file` streams a requested range correctly and does not
    /// regress to a single-buffer approach. A 1 MB file is created and only the first
    /// 1 024 bytes are requested; the response body must be exactly 1 024 bytes.
    #[tokio::test]
    async fn test_serve_mp4_file_range_body_length() {
        use std::io::Write;

        // Write 1 MB of patterned data to a temp file.
        let mut tmp = tempfile::NamedTempFile::new().expect("create tempfile");
        let data: Vec<u8> = (0u8..=255).cycle().take(1024 * 1024).collect();
        tmp.write_all(&data).expect("write tempfile");
        tmp.flush().expect("flush tempfile");

        let file_path = PathBuf::from(tmp.path());

        // Build a minimal Salvo request with a range header.
        let mut req = salvo::Request::new();
        req.headers_mut()
            .insert("range", "bytes=0-1023".parse().unwrap());

        let mut res = salvo::Response::new();
        serve_mp4_file(&file_path, &req, &mut res).await;

        assert_eq!(
            res.status_code,
            Some(salvo::http::StatusCode::PARTIAL_CONTENT)
        );

        let body = res.take_bytes(None).await.expect("collect body");
        assert_eq!(body.len(), 1024, "response body must be exactly 1024 bytes");
        assert_eq!(&body[..], &data[..1024], "response body content must match");
    }

    #[test]
    fn test_basic_range() {
        assert_eq!(parse_byte_range("bytes=0-499", 1000), Ok((0, 499)));
    }

    #[test]
    fn test_range_end_at_last_byte() {
        assert_eq!(parse_byte_range("bytes=0-999", 1000), Ok((0, 999)));
    }

    #[test]
    fn test_open_ended_range() {
        assert_eq!(parse_byte_range("bytes=1000-", 5000), Ok((1000, 4999)));
    }

    #[test]
    fn test_suffix_range() {
        assert_eq!(parse_byte_range("bytes=-500", 1000), Ok((500, 999)));
    }

    #[test]
    fn test_suffix_range_larger_than_file_clamps_to_start() {
        assert_eq!(parse_byte_range("bytes=-1500", 1000), Ok((0, 999)));
    }

    #[test]
    fn test_start_greater_than_end_is_not_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=500-400", 1000),
            Err(RangeError::RangeNotSatisfiable {
                start: 500,
                file_size: 1000
            })
        );
    }

    #[test]
    fn test_start_beyond_file_size_is_not_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=2000-2500", 1000),
            Err(RangeError::RangeNotSatisfiable {
                start: 2000,
                file_size: 1000
            })
        );
    }

    #[test]
    fn test_end_beyond_file_size_is_clamped() {
        assert_eq!(parse_byte_range("bytes=0-2000", 1000), Ok((0, 999)));
    }

    #[test]
    fn test_missing_bytes_prefix() {
        assert_eq!(
            parse_byte_range("invalid=0-100", 1000),
            Err(RangeError::MissingBytesPrefix)
        );
    }

    #[test]
    fn test_non_numeric_bounds() {
        assert_eq!(
            parse_byte_range("bytes=abc-def", 1000),
            Err(RangeError::NonNumericBound)
        );
    }

    #[test]
    fn test_no_dash_is_malformed() {
        assert_eq!(
            parse_byte_range("bytes=0", 1000),
            Err(RangeError::MalformedRange)
        );
    }

    #[test]
    fn test_empty_range_spec_is_malformed() {
        assert_eq!(
            parse_byte_range("bytes=", 1000),
            Err(RangeError::MalformedRange)
        );
    }

    #[test]
    fn test_zero_file_size_is_not_satisfiable() {
        assert_eq!(
            parse_byte_range("bytes=0-0", 0),
            Err(RangeError::RangeNotSatisfiable {
                start: 0,
                file_size: 0
            })
        );
    }
}
