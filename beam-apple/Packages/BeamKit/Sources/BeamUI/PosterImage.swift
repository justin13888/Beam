import BeamDesignSystem
import SwiftUI

/// Artwork, at a fixed aspect ratio, with a placeholder that holds its shape.
///
/// The shape is held before the image arrives on purpose: artwork that resizes
/// its container on load makes a whole grid jump, and a poster grid loading
/// twenty images jumps twenty times.
///
/// Beam serves poster and backdrop URLs straight from the metadata provider's
/// CDN (ADR-0008), so this is an ordinary remote image load with no auth to
/// attach -- unlike everything else the client fetches.
public struct BeamArtwork: View {
    private let url: URL?
    private let aspectRatio: CGFloat
    private let cornerRadius: CGFloat

    /// Load `urlString` at `aspectRatio`.
    public init(
        urlString: String?,
        aspectRatio: CGFloat = BeamTheme.AspectRatio.poster,
        cornerRadius: CGFloat = BeamTheme.Radius.medium
    ) {
        self.url = urlString.flatMap(URL.init(string:))
        self.aspectRatio = aspectRatio
        self.cornerRadius = cornerRadius
    }

    public var body: some View {
        AsyncImage(url: url) { phase in
            switch phase {
            case .success(let image):
                image.resizable().scaledToFill()
            case .failure:
                placeholder(systemImage: "photo")
            case .empty:
                placeholder(systemImage: nil)
            @unknown default:
                placeholder(systemImage: nil)
            }
        }
        .aspectRatio(aspectRatio, contentMode: .fit)
        .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
    }

    private func placeholder(systemImage: String?) -> some View {
        ZStack {
            BeamTheme.Colors.artworkPlaceholder
            if let systemImage {
                Image(systemName: systemImage)
                    .font(.title2)
                    .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
            }
        }
    }
}
