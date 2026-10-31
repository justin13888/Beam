//! A resolution, named the way a viewer would name it.
//!
//! One rule for every client, because a source picker that names the same
//! file differently on two platforms is a bug report waiting to happen: a
//! 1920x804 scope rip was `720p` on Android and `1080p` on Apple when each kept
//! its own.

/// The label for a video of `width` by `height` pixels: `4K`, `1080p`, or the
/// exact dimensions where no name fits. `None` when neither is known.
///
/// Keyed on **width**, not height. A 2.39:1 scope rip of a 1080p master is
/// 1920x804: its height sits in the 720p band, but nobody calls that file
/// 720p, and a source picker that did would have a viewer choose the "higher
/// quality" 1280x720 version of the same film. Width is stable across aspect
/// ratios in a way height is not.
///
/// Lower bounds rather than exact matches, because no two rips of the same
/// master agree on the last few pixels. Below the smallest named band, or with
/// no width at all, the exact dimensions say more than a name that would be
/// wrong.
#[uniffi::export]
#[must_use]
pub fn resolution_label(width: Option<u32>, height: Option<u32>) -> Option<String> {
    let width = width.filter(|width| *width > 0);
    let height = height.filter(|height| *height > 0);
    let named = match width {
        Some(3_400..) => Some("4K"),
        Some(2_400..3_400) => Some("1440p"),
        Some(1_800..2_400) => Some("1080p"),
        Some(1_200..1_800) => Some("720p"),
        Some(640..1_200) => Some("480p"),
        _ => None,
    };
    if let Some(named) = named {
        return Some(named.to_owned());
    }
    match (width, height) {
        (Some(width), Some(height)) => Some(format!("{width}x{height}")),
        (None, Some(height)) => Some(format!("{height}p")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_band_starts_at_its_lower_bound() {
        let cases = [
            (3_400, "4K"),
            (3_399, "1440p"),
            (2_400, "1440p"),
            (2_399, "1080p"),
            (1_800, "1080p"),
            (1_799, "720p"),
            (1_200, "720p"),
            (1_199, "480p"),
            (640, "480p"),
        ];
        for (width, expected) in cases {
            assert_eq!(
                resolution_label(Some(width), Some(480)).as_deref(),
                Some(expected),
                "width {width}"
            );
        }
    }

    #[test]
    fn a_scope_rip_is_named_by_its_width() {
        // Its height alone would put it in the 720p band.
        assert_eq!(
            resolution_label(Some(1_920), Some(804)).as_deref(),
            Some("1080p")
        );
        assert_eq!(
            resolution_label(Some(3_840), Some(1_600)).as_deref(),
            Some("4K")
        );
    }

    #[test]
    fn below_every_band_the_exact_dimensions_are_shown() {
        assert_eq!(
            resolution_label(Some(639), Some(360)).as_deref(),
            Some("639x360")
        );
    }

    #[test]
    fn without_a_width_the_height_is_shown() {
        assert_eq!(
            resolution_label(None, Some(1_080)).as_deref(),
            Some("1080p")
        );
        assert_eq!(
            resolution_label(Some(0), Some(1_080)).as_deref(),
            Some("1080p")
        );
    }

    #[test]
    fn a_small_width_without_a_height_has_no_label() {
        assert_eq!(resolution_label(Some(320), None), None);
        assert_eq!(resolution_label(Some(320), Some(0)), None);
    }

    #[test]
    fn nothing_known_has_no_label() {
        assert_eq!(resolution_label(None, None), None);
        assert_eq!(resolution_label(Some(0), Some(0)), None);
    }

    #[test]
    fn a_named_width_needs_no_height() {
        assert_eq!(
            resolution_label(Some(1_920), None).as_deref(),
            Some("1080p")
        );
    }
}
