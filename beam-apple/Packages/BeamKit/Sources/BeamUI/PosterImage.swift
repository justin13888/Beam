import BeamDesignSystem
import Foundation
import SwiftUI

/// Artwork, at a fixed aspect ratio, with a placeholder that holds its shape.
///
/// The shape is held before the image arrives on purpose: artwork that resizes
/// its container on load makes a whole grid jump, and a poster grid loading
/// twenty images jumps twenty times.
///
/// Beam serves poster and backdrop art itself rather than handing out a
/// provider CDN link (ADR-0015), so this is *not* an ordinary remote image
/// load: the request needs the session cookie, and on a LAN server with a
/// self-signed certificate it needs the trust decision the viewer already
/// made. `AsyncImage` can attach neither, which is why the fetch goes through
/// ``ArtworkLoading`` from the environment instead.
public struct BeamArtwork: View {
    @Environment(\.artworkLoader) private var loader

    private let url: URL?
    private let aspectRatio: CGFloat
    private let cornerRadius: CGFloat

    @State private var image: PlatformImage?
    @State private var failed = false

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
        content
            .aspectRatio(aspectRatio, contentMode: .fit)
            .clipShape(RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
            .task(id: url) { await load() }
    }

    @ViewBuilder
    private var content: some View {
        if let image {
            Image(platformImage: image).resizable().scaledToFill()
        } else if failed || url == nil {
            placeholder(systemImage: "photo")
        } else {
            placeholder(systemImage: nil)
        }
    }

    // `.task` hands back a `@Sendable` closure, so the state writes below are
    // spelled as main-actor work rather than relying on the view's isolation.
    @MainActor
    private func load() async {
        guard let url else { return }
        image = nil
        failed = false

        guard let data = await loader.data(for: url),
            let decoded = PlatformImage(data: data)
        else {
            failed = true
            return
        }
        image = decoded
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

#if canImport(UIKit)
import UIKit

/// The platform's decoded-image type.
typealias PlatformImage = UIImage

extension Image {
    fileprivate init(platformImage: PlatformImage) {
        self.init(uiImage: platformImage)
    }
}
#elseif canImport(AppKit)
import AppKit

/// The platform's decoded-image type.
typealias PlatformImage = NSImage

extension Image {
    fileprivate init(platformImage: PlatformImage) {
        self.init(nsImage: platformImage)
    }
}
#endif
