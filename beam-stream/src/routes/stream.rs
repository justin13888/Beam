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

        if !range_str.starts_with("bytes=") {
            res.status_code(StatusCode::BAD_REQUEST);
            return;
        }

        let range_part = &range_str[6..]; // Remove "bytes="
        let parts: Vec<&str> = range_part.split('-').collect();

        if parts.len() != 2 {
            res.status_code(StatusCode::BAD_REQUEST);
            return;
        }

        let start = if parts[0].is_empty() {
            // Suffix range like "-500"
            if let Ok(suffix) = parts[1].parse::<u64>() {
                file_size.saturating_sub(suffix)
            } else {
                res.status_code(StatusCode::BAD_REQUEST);
                return;
            }
        } else if let Ok(s) = parts[0].parse::<u64>() {
            s
        } else {
            res.status_code(StatusCode::BAD_REQUEST);
            return;
        };

        let end = if parts[1].is_empty() {
            file_size - 1
        } else if let Ok(e) = parts[1].parse::<u64>() {
            std::cmp::min(e, file_size - 1)
        } else {
            res.status_code(StatusCode::BAD_REQUEST);
            return;
        };

        if start > end || start >= file_size {
            res.status_code(StatusCode::RANGE_NOT_SATISFIABLE);
            return;
        }

        (start, end, StatusCode::PARTIAL_CONTENT)
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

    // Read the requested range
    let mut buffer = vec![0u8; content_length as usize];
    match file.read_exact(&mut buffer).await {
        Ok(_) => {}
        Err(err) => {
            error!("Failed to read file: {:?}", err);
            res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
            return;
        }
    }

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

    res.body(salvo::http::body::ResBody::Once(bytes::Bytes::from(buffer)));
}
