import BeamDesignSystem
import SwiftUI

/// A small status chip: a resolution, a codec, "HDR", "Software decode".
///
/// Rendered on glass rather than in a solid colour, so a row of them over
/// artwork stays legible against whatever is behind it without needing a
/// scrim that would dull the artwork.
public struct BeamBadge: View {
    /// How much attention the badge should draw.
    public enum Emphasis: Sendable {
        /// Neutral fact, such as a resolution.
        case neutral
        /// Something good, such as hardware decoding.
        case positive
        /// A caveat worth noticing but not a blocker.
        case caution
        /// A fact that means this will not play here.
        case unavailable
    }

    private let text: String
    private let systemImage: String?
    private let emphasis: Emphasis

    /// A badge reading `text`.
    public init(_ text: String, systemImage: String? = nil, emphasis: Emphasis = .neutral) {
        self.text = text
        self.systemImage = systemImage
        self.emphasis = emphasis
    }

    public var body: some View {
        HStack(spacing: BeamTheme.Spacing.tight) {
            if let systemImage {
                Image(systemName: systemImage)
            }
            Text(text)
        }
        .font(BeamTheme.Typography.badge)
        .foregroundStyle(tint)
        .padding(.horizontal, BeamTheme.Spacing.small)
        .padding(.vertical, BeamTheme.Spacing.tight)
        .beamGlassChip()
    }

    private var tint: Color {
        switch emphasis {
        case .neutral: BeamTheme.Colors.onGlass
        case .positive: BeamTheme.Colors.accent
        case .caution: BeamTheme.Colors.caution
        case .unavailable: BeamTheme.Colors.unavailable
        }
    }
}
