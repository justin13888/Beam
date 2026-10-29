//! A subtitle file beside a video (issue #184).
//!
//! `Movie.en.forced.srt` next to `Movie.mkv` is a subtitle stream of that
//! video that lives in its own file. The indexer records it here -- which
//! video it belongs to, its format, and the language and flags its name
//! carries -- and never writes to it. The subtitle routes serve it, read-only
//! (issue #189).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use uuid::Uuid;

/// The text subtitle formats Beam indexes beside a video. Image-based formats
/// (`.sub`/`.idx`, `.sup`) are not: they cannot be delivered as text
/// (ADR-0004, decision D184-4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SubtitleFormat {
    Srt,
    Vtt,
    Ass,
    Ssa,
}

impl SubtitleFormat {
    /// Every format, for exhaustive checks.
    pub const ALL: [SubtitleFormat; 4] = [
        SubtitleFormat::Srt,
        SubtitleFormat::Vtt,
        SubtitleFormat::Ass,
        SubtitleFormat::Ssa,
    ];

    /// The lowercase file extension, which is also how the format is stored.
    pub fn as_str(self) -> &'static str {
        match self {
            SubtitleFormat::Srt => "srt",
            SubtitleFormat::Vtt => "vtt",
            SubtitleFormat::Ass => "ass",
            SubtitleFormat::Ssa => "ssa",
        }
    }

    /// The FFmpeg name of this format's codec -- the vocabulary an embedded
    /// subtitle stream's codec is recorded in, so a sidecar and an embedded
    /// track of one format read the same (issue #189).
    pub fn codec_name(self) -> &'static str {
        match self {
            SubtitleFormat::Srt => "subrip",
            SubtitleFormat::Vtt => "webvtt",
            SubtitleFormat::Ass => "ass",
            SubtitleFormat::Ssa => "ssa",
        }
    }

    /// The format a file extension names, in any case.
    pub fn from_extension(extension: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|format| extension.eq_ignore_ascii_case(format.as_str()))
    }
}

/// What a sidecar subtitle's filename says about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidecarInfo {
    pub format: SubtitleFormat,
    /// ISO 639-2/B, the code FFmpeg writes into a Matroska track's language
    /// tag -- so a sidecar and an embedded track of one language compare
    /// equal.
    pub language: Option<String>,
    /// What else the name carries: `Commentary`, or the region of a
    /// BCP 47 tag (`pt-BR`).
    pub title: Option<String>,
    pub is_forced: bool,
    /// Subtitles for the deaf and hard of hearing (`sdh`, `cc`, or `hi` after
    /// a language).
    pub is_sdh: bool,
    pub is_default: bool,
}

/// An indexed sidecar subtitle.
#[derive(Debug, Clone, PartialEq)]
pub struct SidecarSubtitle {
    pub id: Uuid,
    /// The video file it belongs to.
    pub file_id: Uuid,
    pub library_id: Uuid,
    pub path: PathBuf,
    pub info: SidecarInfo,
    pub size_bytes: u64,
    pub mtime: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A sidecar subtitle as a scan finds it, keyed by its path.
#[derive(Debug, Clone, PartialEq)]
pub struct UpsertSidecarSubtitle {
    pub file_id: Uuid,
    pub library_id: Uuid,
    pub path: PathBuf,
    pub info: SidecarInfo,
    pub size_bytes: u64,
    pub mtime: Option<DateTime<Utc>>,
}

impl UpsertSidecarSubtitle {
    /// Whether `stored` already records exactly this -- so a scan that finds
    /// the file unchanged writes nothing.
    pub fn matches(&self, stored: &SidecarSubtitle) -> bool {
        let UpsertSidecarSubtitle {
            file_id,
            library_id,
            path,
            info,
            size_bytes,
            mtime,
        } = self;
        stored.file_id == *file_id
            && stored.library_id == *library_id
            && stored.path == *path
            && stored.info == *info
            && stored.size_bytes == *size_bytes
            && stored.mtime == *mtime
    }
}

#[cfg(feature = "entity")]
impl TryFrom<beam_entity::sidecar_subtitle::Model> for SidecarSubtitle {
    type Error = sea_orm::DbErr;

    fn try_from(model: beam_entity::sidecar_subtitle::Model) -> Result<Self, Self::Error> {
        let beam_entity::sidecar_subtitle::Model {
            id,
            file_id,
            library_id,
            path,
            format,
            language,
            title,
            is_forced,
            is_sdh,
            is_default,
            size_bytes,
            mtime,
            created_at,
            updated_at,
        } = model;
        let format = SubtitleFormat::from_extension(&format).ok_or_else(|| {
            sea_orm::DbErr::Custom(format!("unknown sidecar subtitle format {format:?}"))
        })?;
        Ok(Self {
            id,
            file_id,
            library_id,
            path: PathBuf::from(path),
            info: SidecarInfo {
                format,
                language,
                title,
                is_forced,
                is_sdh,
                is_default,
            },
            size_bytes: size_bytes.max(0) as u64,
            mtime: mtime.map(|t| t.with_timezone(&Utc)),
            created_at: created_at.with_timezone(&Utc),
            updated_at: updated_at.with_timezone(&Utc),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_format_is_named_by_its_own_extension_in_any_case() {
        for format in SubtitleFormat::ALL {
            assert_eq!(
                SubtitleFormat::from_extension(format.as_str()),
                Some(format)
            );
            assert_eq!(
                SubtitleFormat::from_extension(&format.as_str().to_ascii_uppercase()),
                Some(format)
            );
        }
        for image_based in ["sub", "idx", "sup", "smi", "nfo"] {
            assert_eq!(SubtitleFormat::from_extension(image_based), None);
        }
    }

    /// Every indexed sidecar is text, which is why it is indexed at all; its
    /// codec name has to say so too, or the sources route would call it an
    /// image and offer no way to read it.
    #[test]
    fn every_format_names_a_distinct_text_codec() {
        let mut seen = std::collections::HashSet::new();
        for format in SubtitleFormat::ALL {
            let codec = format.codec_name();
            assert!(
                crate::utils::subtitle::is_text_subtitle_codec(codec),
                "{format:?} -> {codec}"
            );
            assert!(seen.insert(codec), "{codec} named twice");
        }
    }

    fn upsert() -> UpsertSidecarSubtitle {
        UpsertSidecarSubtitle {
            file_id: Uuid::from_u128(1),
            library_id: Uuid::from_u128(2),
            path: PathBuf::from("/media/Heat.en.forced.srt"),
            info: SidecarInfo {
                format: SubtitleFormat::Srt,
                language: Some("eng".to_string()),
                title: None,
                is_forced: true,
                is_sdh: false,
                is_default: false,
            },
            size_bytes: 4_096,
            mtime: DateTime::from_timestamp(1_700_000_000, 0),
        }
    }

    /// The row `upsert()` was stored as.
    fn stored() -> SidecarSubtitle {
        let UpsertSidecarSubtitle {
            file_id,
            library_id,
            path,
            info,
            size_bytes,
            mtime,
        } = upsert();
        SidecarSubtitle {
            id: Uuid::from_u128(3),
            file_id,
            library_id,
            path,
            info,
            size_bytes,
            mtime,
            created_at: DateTime::UNIX_EPOCH,
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    /// A scan writes a sidecar again exactly when one thing it found differs
    /// from the row -- any of them, each on its own -- and never for what the
    /// store alone assigns.
    #[test]
    fn an_upsert_matches_its_row_exactly_when_every_field_it_carries_agrees() {
        type Change = fn(&mut SidecarSubtitle);
        let cases: [(&str, Change, bool); 13] = [
            ("nothing", |_| {}, true),
            ("id", |s| s.id = Uuid::from_u128(9), true),
            (
                "created_at",
                |s| s.created_at = DateTime::UNIX_EPOCH + chrono::Duration::days(1),
                true,
            ),
            (
                "updated_at",
                |s| s.updated_at = DateTime::UNIX_EPOCH + chrono::Duration::days(1),
                true,
            ),
            ("file_id", |s| s.file_id = Uuid::from_u128(9), false),
            ("library_id", |s| s.library_id = Uuid::from_u128(9), false),
            (
                "path",
                |s| s.path = PathBuf::from("/media/Heat.en.srt"),
                false,
            ),
            ("format", |s| s.info.format = SubtitleFormat::Vtt, false),
            ("language", |s| s.info.language = None, false),
            (
                "title",
                |s| s.info.title = Some("Commentary".to_string()),
                false,
            ),
            ("is_forced", |s| s.info.is_forced = false, false),
            ("size_bytes", |s| s.size_bytes += 1, false),
            ("mtime", |s| s.mtime = None, false),
        ];
        for (field, change, matches) in cases {
            let mut row = stored();
            change(&mut row);
            assert_eq!(upsert().matches(&row), matches, "{field} changed");
        }
        for (field, change) in [
            (
                "is_sdh",
                (|i: &mut SidecarInfo| i.is_sdh = true) as fn(&mut SidecarInfo),
            ),
            ("is_default", |i| i.is_default = true),
        ] {
            let mut row = stored();
            change(&mut row.info);
            assert!(!upsert().matches(&row), "{field} changed");
        }
    }
}

#[cfg(all(test, feature = "entity"))]
mod entity_conversion_tests {
    use super::*;

    fn model(format: &str, size_bytes: i64) -> beam_entity::sidecar_subtitle::Model {
        let at: chrono::DateTime<chrono::FixedOffset> = DateTime::UNIX_EPOCH.into();
        beam_entity::sidecar_subtitle::Model {
            id: Uuid::from_u128(1),
            file_id: Uuid::from_u128(2),
            library_id: Uuid::from_u128(3),
            path: "/media/Heat.en.srt".to_string(),
            format: format.to_string(),
            language: Some("eng".to_string()),
            title: Some("SDH".to_string()),
            is_forced: false,
            is_sdh: true,
            is_default: true,
            size_bytes,
            mtime: Some(at),
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn a_row_reads_back_with_every_field_and_its_format_in_any_case() {
        for stored in ["srt", "SRT"] {
            let read = SidecarSubtitle::try_from(model(stored, 4_096)).unwrap();
            assert_eq!(
                read,
                SidecarSubtitle {
                    id: Uuid::from_u128(1),
                    file_id: Uuid::from_u128(2),
                    library_id: Uuid::from_u128(3),
                    path: PathBuf::from("/media/Heat.en.srt"),
                    info: SidecarInfo {
                        format: SubtitleFormat::Srt,
                        language: Some("eng".to_string()),
                        title: Some("SDH".to_string()),
                        is_forced: false,
                        is_sdh: true,
                        is_default: true,
                    },
                    size_bytes: 4_096,
                    mtime: Some(DateTime::UNIX_EPOCH),
                    created_at: DateTime::UNIX_EPOCH,
                    updated_at: DateTime::UNIX_EPOCH,
                },
                "{stored:?}"
            );
        }
    }

    /// A format the domain does not index -- one the `CHECK` would never let
    /// in -- is refused by name rather than read as some other format.
    #[test]
    fn a_row_of_a_format_beam_does_not_index_is_refused() {
        for stored in ["sub", "", "srt "] {
            let refused = SidecarSubtitle::try_from(model(stored, 1)).unwrap_err();
            assert!(
                matches!(&refused, sea_orm::DbErr::Custom(message) if message.contains(&format!("{stored:?}"))),
                "{stored:?}: {refused}"
            );
        }
    }

    /// A negative size, which no write produces, reads as empty rather than
    /// wrapping to an enormous one.
    #[test]
    fn a_negative_size_reads_as_zero() {
        let read = SidecarSubtitle::try_from(model("vtt", -5)).unwrap();
        assert_eq!(read.size_bytes, 0);
    }
}
