//! Turning a [`LibraryShape`] into the [`LibraryReport`] a collector receives.
//!
//! Pure: the same shape, version and day always give the same report. This is
//! where the report is coarsened -- labels normalised, the total size
//! bucketed -- so nothing downstream can leak more than it says.

use std::collections::BTreeMap;

use beam_domain::models::library_shape::{LibraryShape, NamedCount};
use beam_domain::utils::telemetry::{TotalSizeBucket, normalize_label};
use chrono::NaiveDate;

use crate::models::telemetry::{
    LIBRARY_REPORT_SCHEMA_VERSION, LibraryReport, LibraryReportCodecs, LibraryReportCount,
    LibraryReportFileSize, LibraryReportFiles, LibraryReportTitles,
};

/// Normalises every name and merges the counts of names that normalise alike
/// -- `H264` and `h264` are one codec -- sorted by name.
fn normalized(counts: &[NamedCount]) -> Vec<LibraryReportCount> {
    let mut merged: BTreeMap<String, u64> = BTreeMap::new();
    for NamedCount { name, count } in counts {
        *merged.entry(normalize_label(name)).or_insert(0) += count;
    }
    merged
        .into_iter()
        .map(|(name, count)| LibraryReportCount { name, count })
        .collect()
}

/// The report for `shape`, produced by `server_version` on `generated_on`.
pub fn build_library_report(
    shape: &LibraryShape,
    server_version: &str,
    generated_on: NaiveDate,
) -> LibraryReport {
    let LibraryShape {
        libraries,
        movies,
        shows,
        seasons,
        episodes,
        files,
        containers,
        video_codecs,
        audio_codecs,
        subtitle_codecs,
        file_sizes,
        total_bytes,
    } = shape;

    LibraryReport {
        schema_version: LIBRARY_REPORT_SCHEMA_VERSION,
        server_version: server_version.to_string(),
        generated_on,
        libraries: *libraries,
        titles: LibraryReportTitles {
            movies: *movies,
            shows: *shows,
        },
        seasons: *seasons,
        episodes: *episodes,
        files: LibraryReportFiles {
            movie: files.movie,
            episode: files.episode,
            unclassified: files.unclassified,
        },
        containers: normalized(containers),
        codecs: LibraryReportCodecs {
            video: normalized(video_codecs),
            audio: normalized(audio_codecs),
            subtitle: normalized(subtitle_codecs),
        },
        file_sizes: file_sizes
            .iter()
            .map(|(bucket, count)| LibraryReportFileSize {
                bucket: bucket.into(),
                count,
            })
            .collect(),
        total_size: TotalSizeBucket::of(*total_bytes).into(),
    }
}

#[cfg(test)]
mod tests {
    use beam_domain::models::library_shape::FilesByContentType;
    use beam_domain::utils::telemetry::{FileSizeBucket, FileSizeHistogram, TIB};
    use proptest::prelude::*;

    use super::*;
    use crate::models::telemetry::{LibraryFileSizeBucket, LibraryTotalSizeBucket};

    fn day() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 9, 27).unwrap()
    }

    #[test]
    fn names_that_normalise_alike_are_one_entry() {
        let shape = LibraryShape {
            video_codecs: vec![
                NamedCount::new("H264", 2),
                NamedCount::new("HEVC", 1),
                NamedCount::new("h264", 3),
            ],
            subtitle_codecs: vec![
                NamedCount::new("Other(\"hdmv_pgs_subtitle\")", 1),
                NamedCount::new("None", 4),
            ],
            ..LibraryShape::default()
        };

        let report = build_library_report(&shape, "1.2.3", day());

        assert_eq!(
            report.codecs.video,
            vec![
                LibraryReportCount {
                    name: "h264".to_string(),
                    count: 5
                },
                LibraryReportCount {
                    name: "hevc".to_string(),
                    count: 1
                },
            ]
        );
        assert_eq!(
            report.codecs.subtitle,
            vec![
                LibraryReportCount {
                    name: "hdmv_pgs_subtitle".to_string(),
                    count: 1
                },
                LibraryReportCount {
                    name: "unknown".to_string(),
                    count: 4
                },
            ]
        );
    }

    #[test]
    fn the_total_size_is_reported_as_a_bucket_never_exactly() {
        let shape = LibraryShape {
            total_bytes: 3 * TIB + 12_345,
            ..LibraryShape::default()
        };

        let report = build_library_report(&shape, "1.2.3", day());
        let json = serde_json::to_string(&report).unwrap();

        assert_eq!(report.total_size, LibraryTotalSizeBucket::From1To10Tib);
        assert!(
            !json.contains(&(3 * TIB + 12_345).to_string()),
            "the exact byte count must not appear: {json}"
        );
    }

    #[test]
    fn every_size_bucket_is_listed_smallest_first_even_when_empty() {
        let mut histogram = FileSizeHistogram::default();
        histogram.record(FileSizeBucket::From10To25Gib.lower_bound_bytes());

        let report = build_library_report(
            &LibraryShape {
                file_sizes: histogram,
                files: FilesByContentType {
                    movie: 1,
                    ..FilesByContentType::default()
                },
                ..LibraryShape::default()
            },
            "1.2.3",
            day(),
        );

        let listed: Vec<LibraryFileSizeBucket> =
            report.file_sizes.iter().map(|size| size.bucket).collect();
        let expected: Vec<LibraryFileSizeBucket> =
            FileSizeBucket::ALL.into_iter().map(Into::into).collect();
        assert_eq!(listed, expected);
        assert_eq!(
            report
                .file_sizes
                .iter()
                .map(|size| size.count)
                .collect::<Vec<_>>(),
            vec![0, 0, 0, 1, 0, 0]
        );
    }

    proptest! {
        /// However the names arrive, normalising merges without losing or
        /// inventing a stream.
        #[test]
        fn normalising_preserves_the_total_count(
            names in proptest::collection::vec((".{0,12}", 0u64..1000), 0..16)
        ) {
            let counts: Vec<NamedCount> =
                names.iter().map(|(name, count)| NamedCount::new(name.clone(), *count)).collect();
            let merged = normalized(&counts);

            let before: u64 = counts.iter().map(|c| c.count).sum();
            let after: u64 = merged.iter().map(|c| c.count).sum();
            prop_assert_eq!(before, after);
            let mut names: Vec<&str> = merged.iter().map(|c| c.name.as_str()).collect();
            let listed = names.clone();
            names.sort_unstable();
            names.dedup();
            prop_assert_eq!(names, listed, "sorted, with no name twice");
        }
    }
}
