import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import SwiftUI

/// One title in a grid or a row.
public struct MediaCard: View {
    private let title: String
    private let subtitle: String?
    private let artworkURL: String?
    private let progress: Double?

    /// A card for `title`.
    ///
    /// - Parameter progress: a fraction in `0...1` to draw as a resume bar,
    ///   or `nil` for a title not yet started.
    public init(title: String, subtitle: String?, artworkURL: String?, progress: Double? = nil) {
        self.title = title
        self.subtitle = subtitle
        self.artworkURL = artworkURL
        self.progress = progress
    }

    /// A card for a catalogue entry.
    public init(_ summary: MediaSummary) {
        self.init(
            title: summary.title,
            subtitle: summary.year.map(String.init),
            artworkURL: summary.posterUrl
        )
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
            ZStack(alignment: .bottom) {
                BeamArtwork(urlString: artworkURL)
                if let progress, progress > 0 {
                    ProgressView(value: min(max(progress, 0), 1))
                        .progressViewStyle(.linear)
                        .tint(BeamTheme.Colors.accent)
                        .padding(.horizontal, BeamTheme.Spacing.small)
                        .padding(.bottom, BeamTheme.Spacing.small)
                }
            }

            Text(title)
                .font(BeamTheme.Typography.cardTitle)
                .lineLimit(2)
                .multilineTextAlignment(.leading)

            if let subtitle {
                Text(subtitle)
                    .font(BeamTheme.Typography.cardSubtitle)
                    .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(accessibilityLabel)
    }

    private var accessibilityLabel: String {
        var parts = [title]
        if let subtitle { parts.append(subtitle) }
        if let progress, progress > 0 {
            parts.append("\(Int(progress * 100)) percent watched")
        }
        return parts.joined(separator: ", ")
    }
}

/// A wide card for the continue-watching row.
///
/// Backdrop-shaped rather than poster-shaped on purpose: it is a resume
/// affordance, and a still from partway through reads as "you were here"
/// where a poster reads as "start this".
public struct ContinueWatchingCard: View {
    private let entry: ContinueWatchingEntry

    /// A card for `entry`.
    public init(_ entry: ContinueWatchingEntry) {
        self.entry = entry
    }

    public var body: some View {
        VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
            ZStack(alignment: .bottomLeading) {
                BeamArtwork(
                    urlString: entry.artworkURL,
                    aspectRatio: BeamTheme.AspectRatio.backdrop
                )

                HStack {
                    BeamBadge(
                        BeamFormat.duration(seconds: remainingSeconds),
                        systemImage: "clock",
                        emphasis: .neutral
                    )
                    Spacer()
                }
                .padding(BeamTheme.Spacing.small)
            }

            ProgressView(value: entry.fraction)
                .progressViewStyle(.linear)
                .tint(BeamTheme.Colors.accent)

            Text(entry.displayTitle)
                .font(BeamTheme.Typography.cardTitle)
                .lineLimit(1)

            if let subtitle = entry.displaySubtitle {
                Text(subtitle)
                    .font(BeamTheme.Typography.cardSubtitle)
                    .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(
            "\(entry.displayTitle), \(Int(entry.fraction * 100)) percent watched"
        )
    }

    private var remainingSeconds: Double? {
        guard let duration = entry.durationSecs, duration > 0 else { return nil }
        return max(0, duration - entry.positionSecs)
    }
}
