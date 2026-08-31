import SwiftUI

/// The Liquid Glass vocabulary, applied as a system rather than per view.
///
/// Two rules hold everywhere in Beam and are what these modifiers encode:
///
/// 1. **Glass floats over content, never under it.** Artwork is the content;
///    the controls above it are glass. Tinting a poster with a glass material
///    would be using the material as decoration, which is what makes an
///    interface look busy rather than layered.
/// 2. **Related controls share one container.** `GlassEffectContainer` is what
///    lets adjacent glass elements merge as they approach and separate as they
///    part. Individually glassed buttons sitting side by side do not do that,
///    and the difference is immediately visible in motion.
extension View {
    /// A floating glass panel: the default treatment for a control cluster.
    ///
    /// - Parameters:
    ///   - shape: the panel's outline. A capsule for a pill of controls, a
    ///     rounded rectangle for a card-shaped one.
    ///   - interactive: whether the panel should respond to touch with the
    ///     system's own highlight. Use for anything tappable, and not for a
    ///     panel that merely groups.
    public func beamGlassPanel(
        shape: some Shape = RoundedRectangle(
            cornerRadius: BeamTheme.Radius.large,
            style: .continuous
        ),
        interactive: Bool = false
    ) -> some View {
        glassEffect(interactive ? .regular.interactive() : .regular, in: shape)
    }

    /// A glass chip, for badges and small status pills.
    public func beamGlassChip() -> some View {
        glassEffect(.regular, in: Capsule())
    }

    /// Standard padding inside a glass panel.
    ///
    /// Glass needs more inset than an opaque surface: the material's edge is
    /// where its refraction is strongest, and content set too close to it is
    /// harder to read.
    public func beamGlassPadding() -> some View {
        padding(.horizontal, BeamTheme.Spacing.regular)
            .padding(.vertical, BeamTheme.Spacing.compact)
    }

    /// Let content run under the top and bottom edges with a soft fade, which
    /// is what makes a scrolling list look like it passes beneath the glass
    /// chrome rather than stopping short of it.
    public func beamScrollEdges() -> some View {
        scrollEdgeEffectStyle(.soft, for: .all)
    }
}

/// Groups related glass elements so they merge and separate as one system.
///
/// A thin wrapper over `GlassEffectContainer`, existing so the spacing that
/// governs when elements merge is one value rather than a number repeated at
/// every call site.
public struct BeamGlassGroup<Content: View>: View {
    private let spacing: CGFloat
    private let content: Content

    /// Group `content`.
    ///
    /// - Parameter spacing: how close two elements must come before they
    ///   merge. Defaults to the compact step, which reads as "these belong
    ///   together" without gluing everything on screen into one blob.
    public init(spacing: CGFloat = BeamTheme.Spacing.compact, @ViewBuilder content: () -> Content) {
        self.spacing = spacing
        self.content = content()
    }

    public var body: some View {
        GlassEffectContainer(spacing: spacing) {
            content
        }
    }
}
