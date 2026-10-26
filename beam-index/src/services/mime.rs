//! The registered media type a probed container is stored and served as.
//!
//! FFmpeg reports a container as the comma-separated list of demuxers that
//! matched it (`mov,mp4,m4a,3gp,3g2,mj2` for an MP4, `matroska,webm` for both
//! Matroska and WebM), which is not a media type. [`container_mime_type`] maps
//! that list onto the IANA-registered type a browser or platform player
//! recognises, falling back to `application/octet-stream` for a container with
//! no mapping.

use std::path::Path;

/// The type a container with no row in [`CONTAINER_MIME_TYPES`] is served as.
pub const UNKNOWN_CONTAINER_MIME_TYPE: &str = "application/octet-stream";

/// One mapping: a demuxer name FFmpeg lists, an optional file extension that
/// must also match (for demuxer lists FFmpeg shares between containers), and
/// the media type.
struct ContainerMimeType {
    demuxer: &'static str,
    extension: Option<&'static str>,
    mime_type: &'static str,
}

/// Rows are tried in order and the first match wins, so a row that also
/// requires an extension comes before the demuxer's general row.
const CONTAINER_MIME_TYPES: &[ContainerMimeType] = &[
    // FFmpeg's one `matroska,webm` demuxer reads both; only the name tells them apart.
    ContainerMimeType {
        demuxer: "webm",
        extension: Some("webm"),
        mime_type: "video/webm",
    },
    ContainerMimeType {
        demuxer: "matroska",
        extension: None,
        mime_type: "video/x-matroska",
    },
    ContainerMimeType {
        demuxer: "webm",
        extension: None,
        mime_type: "video/webm",
    },
    // Likewise `mov,mp4,m4a,3gp,3g2,mj2` reads QuickTime and every ISO BMFF brand.
    ContainerMimeType {
        demuxer: "mov",
        extension: Some("mov"),
        mime_type: "video/quicktime",
    },
    ContainerMimeType {
        demuxer: "mp4",
        extension: None,
        mime_type: "video/mp4",
    },
    // MPEG transport stream, including BDAV `.m2ts`.
    ContainerMimeType {
        demuxer: "mpegts",
        extension: None,
        mime_type: "video/mp2t",
    },
    // MPEG program stream.
    ContainerMimeType {
        demuxer: "mpeg",
        extension: None,
        mime_type: "video/mpeg",
    },
];

/// The registered media type of a file whose container FFmpeg reported as
/// `format_name`, read from `path`.
pub fn container_mime_type(format_name: &str, path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let demuxers = format_name.split(',').map(str::trim);
    CONTAINER_MIME_TYPES
        .iter()
        .find(|row| {
            let ContainerMimeType {
                demuxer,
                extension: required_extension,
                mime_type: _,
            } = row;
            let extension_matches =
                required_extension.is_none_or(|required| extension.as_deref() == Some(required));
            extension_matches && demuxers.clone().any(|name| name == *demuxer)
        })
        .map_or(UNKNOWN_CONTAINER_MIME_TYPE, |row| row.mime_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real `format_name` values FFmpeg reports, with the file name each was
    /// read from and the type it must be stored as.
    #[test]
    fn maps_ffmpeg_demuxer_lists_to_registered_types() {
        let cases: &[(&str, &str, &str)] = &[
            ("mov,mp4,m4a,3gp,3g2,mj2", "film.mp4", "video/mp4"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "film.m4v", "video/mp4"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "film.mov", "video/quicktime"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "FILM.MOV", "video/quicktime"),
            ("mov,mp4,m4a,3gp,3g2,mj2", "film", "video/mp4"),
            ("matroska,webm", "film.mkv", "video/x-matroska"),
            ("matroska,webm", "film.webm", "video/webm"),
            ("matroska,webm", "film.WebM", "video/webm"),
            ("matroska,webm", "film", "video/x-matroska"),
            ("matroska", "film.mkv", "video/x-matroska"),
            ("webm", "film.mkv", "video/webm"),
            ("mpegts", "film.ts", "video/mp2t"),
            ("mpegts", "00001.m2ts", "video/mp2t"),
            ("mpeg", "VTS_01_1.VOB", "video/mpeg"),
            ("mpeg", "film.mpg", "video/mpeg"),
            ("avi", "film.avi", UNKNOWN_CONTAINER_MIME_TYPE),
            ("flv", "film.flv", UNKNOWN_CONTAINER_MIME_TYPE),
            ("", "film.mp4", UNKNOWN_CONTAINER_MIME_TYPE),
        ];
        for &(format_name, file_name, expected) in cases {
            assert_eq!(
                container_mime_type(format_name, Path::new(file_name)),
                expected,
                "{format_name:?} read from {file_name:?}",
            );
        }
    }

    #[test]
    fn extension_row_needs_its_demuxer() {
        // A `.webm` name on an MP4 does not make it WebM.
        assert_eq!(
            container_mime_type("mov,mp4,m4a,3gp,3g2,mj2", Path::new("film.webm")),
            "video/mp4"
        );
    }
}
