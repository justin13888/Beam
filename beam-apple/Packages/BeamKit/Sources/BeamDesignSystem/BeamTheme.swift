import SwiftUI

/// The design tokens every Beam surface is built from.
///
/// Tokens rather than literals scattered through views, for the reason every
/// design system exists: a spacing value used in eleven places is a value that
/// will be changed in nine of them. Mirrors `core/designsystem` on Android, so
/// the two clients can be compared side by side and disagree visibly rather
/// than by drift.
public enum BeamTheme {
    /// Corner radii, in points.
    public enum Radius {
        /// Badges and other small chips.
        public static let small: CGFloat = 8
        /// Cards and list rows.
        public static let medium: CGFloat = 14
        /// Sheets, hero panels and the player's control cluster.
        public static let large: CGFloat = 24
    }

    /// Spacing, in points. A four-point scale, so nothing lands off-grid.
    public enum Spacing {
        /// 4pt.
        public static let tight: CGFloat = 4
        /// 8pt.
        public static let small: CGFloat = 8
        /// 12pt.
        public static let compact: CGFloat = 12
        /// 16pt -- the default gutter.
        public static let regular: CGFloat = 16
        /// 24pt.
        public static let loose: CGFloat = 24
        /// 32pt, between sections.
        public static let section: CGFloat = 32
    }

    /// Poster and backdrop proportions, as width over height.
    public enum AspectRatio {
        /// The standard poster shape.
        public static let poster: CGFloat = 2.0 / 3.0
        /// The standard backdrop shape.
        public static let backdrop: CGFloat = 16.0 / 9.0
    }

    /// Colours, expressed against the system palette.
    ///
    /// Deliberately not a bespoke palette. Liquid Glass derives its tint and
    /// its legibility from what is behind it, and a hardcoded colour fights
    /// that -- it stops adapting to the wallpaper, to dark mode, and to the
    /// accessibility settings that increase contrast or reduce transparency.
    public enum Colors {
        /// The accent Beam uses for playable, selected and in-progress state.
        public static let accent = Color.accentColor
        /// Text on a glass surface.
        public static let onGlass = Color.primary
        /// Secondary text on a glass surface.
        public static let onGlassSecondary = Color.secondary
        /// The plate behind artwork before it loads.
        public static let artworkPlaceholder = Color.secondary.opacity(0.15)
        /// Warnings that are informational rather than failures -- a source
        /// that will play but only in software, say.
        public static let caution = Color.orange
        /// A source that cannot play here at all.
        public static let unavailable = Color.secondary
    }

    /// Type styles, as semantic roles rather than sizes, so Dynamic Type keeps
    /// working.
    public enum Typography {
        /// A screen's own title.
        public static let screenTitle = Font.largeTitle.weight(.bold)
        /// A section heading in a scroll view.
        public static let sectionTitle = Font.title3.weight(.semibold)
        /// A card's title.
        public static let cardTitle = Font.subheadline.weight(.medium)
        /// A card's second line.
        public static let cardSubtitle = Font.caption
        /// Text inside a badge.
        public static let badge = Font.caption2.weight(.semibold)
    }
}
