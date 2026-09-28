//! Whether a playback progress report is one the server records (issue #188).
//!
//! A report says where the viewer is, and optionally how long the title is.
//! What the server can check is that both are numbers a player could have
//! produced: a position at or after the start, a positive duration, and a
//! position no further than the end. The end is the duration the report
//! names, else the one the file was probed at.
//!
//! A player's clock and a container's declared duration rarely agree to the
//! frame, so a position slightly past the end is the end: up to
//! [`END_TOLERANCE_SECS`] or [`END_TOLERANCE_FRACTION`] of the duration,
//! whichever is larger, is clamped to it rather than refused. Anything
//! further is a bug in the client or a report for another file, and is
//! refused so it cannot be stored as a resume point no player can seek to.

/// The absolute slack past the end a position may carry: two seconds.
pub const END_TOLERANCE_SECS: f64 = 2.0;
/// The relative slack past the end a position may carry: one percent.
pub const END_TOLERANCE_FRACTION: f64 = 0.01;

/// A report the server records: the position, clamped to the end, and the
/// duration it is measured against, when one is known.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValidReport {
    pub position_secs: f64,
    pub duration_secs: Option<f64>,
}

/// One rule a report breaks, at the JSON Pointer of the member that broke it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportFault {
    pub pointer: &'static str,
    pub detail: String,
}

/// How far past `end` a position may be and still be the end.
#[must_use]
pub fn end_tolerance(end: f64) -> f64 {
    END_TOLERANCE_SECS.max(end * END_TOLERANCE_FRACTION)
}

/// Check a report of `position_secs` into a title of `reported_duration`,
/// played from a file probed at `file_duration`.
///
/// # Errors
///
/// Every rule the report breaks, each at the member that broke it.
pub fn validate_report(
    position_secs: f64,
    reported_duration: Option<f64>,
    file_duration: Option<f64>,
) -> Result<ValidReport, Vec<ReportFault>> {
    let mut faults = Vec::new();
    if !(position_secs.is_finite() && position_secs >= 0.0) {
        faults.push(ReportFault {
            pointer: "/position_secs",
            detail: format!("the position must be a number of seconds from 0, not {position_secs}"),
        });
    }
    if let Some(duration) = reported_duration
        && !(duration.is_finite() && duration > 0.0)
    {
        faults.push(ReportFault {
            pointer: "/duration_secs",
            detail: format!("the duration must be a positive number of seconds, not {duration}"),
        });
    }
    if !faults.is_empty() {
        return Err(faults);
    }

    // A probe that read no length, or a nonsensical one, is no end to hold
    // the report to.
    let end = reported_duration.or(file_duration.filter(|d| d.is_finite() && *d > 0.0));
    let Some(end) = end else {
        return Ok(ValidReport {
            position_secs,
            duration_secs: None,
        });
    };
    if position_secs > end + end_tolerance(end) {
        return Err(vec![ReportFault {
            pointer: "/position_secs",
            detail: format!("the position {position_secs}s is past the end of a {end}s title"),
        }]);
    }
    Ok(ValidReport {
        position_secs: position_secs.min(end),
        duration_secs: Some(end),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn pointers(result: Result<ValidReport, Vec<ReportFault>>) -> Vec<&'static str> {
        result
            .expect_err("the report is refused")
            .into_iter()
            .map(|fault| fault.pointer)
            .collect()
    }

    #[test]
    fn the_table() {
        let ok = |position, duration| {
            Ok::<_, Vec<ReportFault>>(ValidReport {
                position_secs: position,
                duration_secs: duration,
            })
        };
        let accepted = [
            ((0.0, Some(100.0), None), ok(0.0, Some(100.0))),
            ((50.0, Some(100.0), Some(7200.0)), ok(50.0, Some(100.0))),
            // No duration in the report: the file's is the end.
            ((50.0, None, Some(100.0)), ok(50.0, Some(100.0))),
            // Neither: any non-negative position is taken as it is.
            ((1e9, None, None), ok(1e9, None)),
            // A probed duration that is no length is no end.
            ((50.0, None, Some(0.0)), ok(50.0, None)),
            // Just past the end is the end: two seconds on a short title...
            ((101.5, Some(100.0), None), ok(100.0, Some(100.0))),
            ((102.0, Some(100.0), None), ok(100.0, Some(100.0))),
            // ...one percent on a long one.
            ((7260.0, Some(7200.0), None), ok(7200.0, Some(7200.0))),
        ];
        for ((position, reported, file), expected) in accepted {
            assert_eq!(
                validate_report(position, reported, file),
                expected,
                "{position} of {reported:?} / {file:?}"
            );
        }

        assert_eq!(
            pointers(validate_report(-5.0, Some(100.0), None)),
            ["/position_secs"]
        );
        assert_eq!(
            pointers(validate_report(f64::NAN, None, None)),
            ["/position_secs"]
        );
        assert_eq!(
            pointers(validate_report(f64::INFINITY, None, None)),
            ["/position_secs"]
        );
        assert_eq!(
            pointers(validate_report(10.0, Some(0.0), None)),
            ["/duration_secs"]
        );
        assert_eq!(
            pointers(validate_report(10.0, Some(-1.0), None)),
            ["/duration_secs"]
        );
        assert_eq!(
            pointers(validate_report(-1.0, Some(f64::NAN), None)),
            ["/position_secs", "/duration_secs"],
            "every broken rule is reported, not only the first"
        );
        assert_eq!(
            pointers(validate_report(102.1, Some(100.0), None)),
            ["/position_secs"]
        );
        assert_eq!(
            pointers(validate_report(7272.1, Some(7200.0), None)),
            ["/position_secs"]
        );
        // The file's end holds a report that names none.
        assert_eq!(
            pointers(validate_report(200.0, None, Some(100.0))),
            ["/position_secs"]
        );
    }

    proptest! {
        /// An accepted report is never past its end, never before the
        /// start, and a report within the end is taken exactly.
        #[test]
        fn an_accepted_position_lies_within_the_title(
            position in -10.0f64..20_000.0,
            reported in prop::option::of(-10.0f64..10_000.0),
            file in prop::option::of(-10.0f64..10_000.0),
        ) {
            if let Ok(report) = validate_report(position, reported, file) {
                prop_assert!(report.position_secs >= 0.0);
                if let Some(end) = report.duration_secs {
                    prop_assert!(end > 0.0);
                    prop_assert!(report.position_secs <= end);
                    if position <= end {
                        prop_assert_eq!(report.position_secs, position);
                    }
                }
            }
        }
    }
}
