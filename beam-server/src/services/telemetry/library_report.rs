//! Turning a [`LibraryShape`] into the [`LibraryReport`] a collector receives.
//!
//! Pure: the same shape, version and day always give the same report. This is
//! where the report is coarsened -- labels normalised, every count and the
//! total size bucketed -- so nothing downstream can leak more than it says.

use std::collections::BTreeMap;

use beam_domain::models::library_shape::{LibraryShape, NamedCount};
use beam_domain::utils::telemetry::{TotalSizeBucket, normalize_label};
use chrono::NaiveDate;

use crate::models::telemetry::{
    LIBRARY_REPORT_SCHEMA_VERSION, LibraryCountBucket, LibraryReport, LibraryReportCodecs,
    LibraryReportCount, LibraryReportFileSize, LibraryReportFiles, LibraryReportTitles,
};

/// Normalises every name and merges the exact counts of names that normalise
/// alike -- `H264` and `h264` are one codec -- keyed, so sorted, by name.
fn merged(counts: &[NamedCount]) -> BTreeMap<String, u64> {
    let mut merged: BTreeMap<String, u64> = BTreeMap::new();
    for NamedCount { name, count } in counts {
        *merged.entry(normalize_label(name)).or_insert(0) += count;
    }
    merged
}

/// [`merged`], each total then bucketed. Merged first: `H264` ×6 and `h264`
/// ×5 are one codec with 11 streams, not two entries of `from_1_to_9`.
fn normalized(counts: &[NamedCount]) -> Vec<LibraryReportCount> {
    merged(counts)
        .into_iter()
        .map(|(name, count)| LibraryReportCount {
            name,
            count: LibraryCountBucket::of(count),
        })
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
        libraries: LibraryCountBucket::of(*libraries),
        titles: LibraryReportTitles {
            movies: LibraryCountBucket::of(*movies),
            shows: LibraryCountBucket::of(*shows),
        },
        seasons: LibraryCountBucket::of(*seasons),
        episodes: LibraryCountBucket::of(*episodes),
        files: LibraryReportFiles {
            movie: LibraryCountBucket::of(files.movie),
            episode: LibraryCountBucket::of(files.episode),
            unclassified: LibraryCountBucket::of(files.unclassified),
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
                count: LibraryCountBucket::of(count),
            })
            .collect(),
        total_size: TotalSizeBucket::of(*total_bytes).into(),
    }
}

#[cfg(test)]
mod tests {
    use beam_domain::models::library_shape::FilesByContentType;
    use beam_domain::utils::telemetry::{FileSizeBucket, FileSizeHistogram, GIB, TIB};
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
                NamedCount::new("H264", 6),
                NamedCount::new("HEVC", 1),
                NamedCount::new("h264", 5),
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
                // 6 + 5: merged, then bucketed.
                LibraryReportCount {
                    name: "h264".to_string(),
                    count: LibraryCountBucket::of(11)
                },
                LibraryReportCount {
                    name: "hevc".to_string(),
                    count: LibraryCountBucket::of(1)
                },
            ]
        );
        assert_eq!(
            report.codecs.subtitle,
            vec![
                LibraryReportCount {
                    name: "hdmv_pgs_subtitle".to_string(),
                    count: LibraryCountBucket::of(1)
                },
                LibraryReportCount {
                    name: "unknown".to_string(),
                    count: LibraryCountBucket::of(4)
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
        for size in &report.file_sizes {
            let expected = if size.bucket == FileSizeBucket::From10To25Gib.into() {
                1
            } else {
                0
            };
            assert_eq!(size.count, LibraryCountBucket::of(expected), "{size:?}");
        }
    }

    /// Issue #93 asked for a coarse shape: no count leaves exactly. No count
    /// here is a substring of anything else the report carries -- a bucket
    /// name, the version, the day -- so any of them appearing is that count
    /// leaking.
    #[test]
    fn no_count_is_reported_exactly() {
        let counts = [
            2_345u64, 3_456, 4_567, 5_678, 6_789, 23_456, 34_567, 45_678, 56_789, 67_892, 78_923,
            89_234,
        ];
        let mut file_sizes = FileSizeHistogram::default();
        for _ in 0..counts[11] {
            file_sizes.record(GIB);
        }
        let shape = LibraryShape {
            libraries: counts[0],
            movies: counts[1],
            shows: counts[2],
            seasons: counts[3],
            episodes: counts[4],
            files: FilesByContentType {
                movie: counts[5],
                episode: counts[6],
                unclassified: counts[7],
            },
            containers: vec![NamedCount::new("matroska,webm", counts[8])],
            video_codecs: vec![NamedCount::new("h264", counts[9])],
            audio_codecs: vec![NamedCount::new("aac", counts[10])],
            subtitle_codecs: Vec::new(),
            file_sizes,
            total_bytes: 0,
        };

        let report = build_library_report(&shape, "1.2.3", day());
        let json = serde_json::to_string(&report).unwrap();

        for count in counts {
            assert!(
                !json.contains(&count.to_string()),
                "{count} leaked into {json}"
            );
        }
        assert_eq!(report.libraries, LibraryCountBucket::of(counts[0]));
        assert_eq!(report.episodes, LibraryCountBucket::of(counts[4]));
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
            let totals = merged(&counts);

            let before: u64 = counts.iter().map(|c| c.count).sum();
            let after: u64 = totals.values().sum();
            prop_assert_eq!(before, after);

            // What is reported is exactly the merged totals, bucketed, in
            // name order.
            let reported = normalized(&counts);
            let expected: Vec<(String, LibraryCountBucket)> = totals
                .into_iter()
                .map(|(name, count)| (name, LibraryCountBucket::of(count)))
                .collect();
            let listed: Vec<(String, LibraryCountBucket)> =
                reported.into_iter().map(|c| (c.name, c.count)).collect();
            prop_assert_eq!(listed, expected, "sorted, with no name twice");
        }
    }
}
