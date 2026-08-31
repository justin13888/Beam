import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// The first screen after signing in.
public struct HomeScreen: View {
    @State private var model: HomeModel
    private let onOpenTitle: (String) -> Void
    private let onResume: (PlaybackRequest) -> Void

    /// Build the screen over a model.
    ///
    /// Navigation arrives as closures rather than as a router this feature
    /// imports, which is what keeps features from depending on each other --
    /// the same rule `beam-android`'s convention plugin enforces.
    public init(
        model: HomeModel,
        onOpenTitle: @escaping (String) -> Void,
        onResume: @escaping (PlaybackRequest) -> Void
    ) {
        _model = State(wrappedValue: model)
        self.onOpenTitle = onOpenTitle
        self.onResume = onResume
    }

    public var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: BeamTheme.Spacing.section) {
                continueWatchingRow
                mediaRow("Recently added", state: model.recentlyAdded)
                mediaRow("Top rated", state: model.topRated)
            }
            .padding(.vertical, BeamTheme.Spacing.regular)
        }
        .beamScrollEdges()
        .navigationTitle("Home")
        .task { await model.load() }
        .refreshable { await model.load() }
    }

    @ViewBuilder
    private var continueWatchingRow: some View {
        switch model.continueWatching {
        case .idle, .loading:
            SectionHeader(title: "Continue watching")
            ProgressView().padding(.horizontal, BeamTheme.Spacing.regular)
        case .failed(let message):
            SectionHeader(title: "Continue watching")
            Text(message)
                .font(.footnote)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                .padding(.horizontal, BeamTheme.Spacing.regular)
        case .loaded(let entries) where entries.isEmpty:
            // Nothing part-watched is the normal state for a new library, not
            // an error and not worth a row of empty space.
            EmptyView()
        case .loaded(let entries):
            SectionHeader(title: "Continue watching")
            ScrollView(.horizontal) {
                LazyHStack(spacing: BeamTheme.Spacing.regular) {
                    ForEach(entries, id: \.fileId) { entry in
                        Button {
                            onResume(entry.playbackRequest)
                        } label: {
                            ContinueWatchingCard(entry).frame(width: 280)
                        }
                        .buttonStyle(.plain)
                        .contextMenu {
                            Button("Go to title") { onOpenTitle(entry.mediaId) }
                        }
                    }
                }
                .padding(.horizontal, BeamTheme.Spacing.regular)
            }
            .scrollClipDisabled()
        }
    }

    @ViewBuilder
    private func mediaRow(_ title: String, state: LoadState<[MediaSummary]>) -> some View {
        SectionHeader(title: title)
        switch state {
        case .idle, .loading:
            ProgressView().padding(.horizontal, BeamTheme.Spacing.regular)
        case .failed(let message):
            Text(message)
                .font(.footnote)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                .padding(.horizontal, BeamTheme.Spacing.regular)
        case .loaded(let items):
            ScrollView(.horizontal) {
                LazyHStack(spacing: BeamTheme.Spacing.regular) {
                    ForEach(items, id: \.id) { item in
                        Button {
                            onOpenTitle(item.id)
                        } label: {
                            MediaCard(item).frame(width: 140)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, BeamTheme.Spacing.regular)
            }
            .scrollClipDisabled()
        }
    }
}

/// A section heading in a scrolling shelf.
struct SectionHeader: View {
    let title: String

    var body: some View {
        Text(title)
            .font(BeamTheme.Typography.sectionTitle)
            .padding(.horizontal, BeamTheme.Spacing.regular)
    }
}
