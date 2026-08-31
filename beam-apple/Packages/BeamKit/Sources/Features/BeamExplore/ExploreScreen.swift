import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// Browse and search the whole catalogue.
public struct ExploreScreen: View {
    @State private var model: ExploreModel
    private let title: String
    private let onOpenTitle: (String) -> Void

    /// Build the screen over a model.
    public init(
        model: ExploreModel,
        title: String = "Explore",
        onOpenTitle: @escaping (String) -> Void
    ) {
        _model = State(wrappedValue: model)
        self.title = title
        self.onOpenTitle = onOpenTitle
    }

    private static let columns = [
        GridItem(.adaptive(minimum: 120, maximum: 180), spacing: BeamTheme.Spacing.regular)
    ]

    public var body: some View {
        content
            .navigationTitle(title)
            .searchable(text: $model.searchText, prompt: "Search titles")
            .toolbar { filterMenu }
            .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.results {
        case .idle, .loading:
            BeamStateView(.loading)
        case .failed(let message):
            BeamStateView(.failed(message: message, isRetryable: true)) {
                Task { await model.load() }
            }
        case .loaded(let items) where items.isEmpty:
            BeamStateView(
                .empty(
                    title: "Nothing found",
                    message: "Try a different search or clear the filters.",
                    systemImage: "magnifyingglass"
                )
            )
        case .loaded(let items):
            ScrollView {
                LazyVGrid(columns: Self.columns, spacing: BeamTheme.Spacing.loose) {
                    ForEach(items, id: \.id) { item in
                        Button {
                            onOpenTitle(item.id)
                        } label: {
                            MediaCard(item)
                        }
                        .buttonStyle(.plain)
                        .onAppear {
                            // Prefetch when the last card appears rather than
                            // on a scroll-offset threshold: the offset is
                            // wrong on every layout the grid can adapt to.
                            if item.id == items.last?.id { model.loadMore() }
                        }
                    }
                }
                .padding(BeamTheme.Spacing.regular)

                if model.isLoadingMore {
                    ProgressView().padding()
                }
            }
            .beamScrollEdges()
        }
    }

    @ToolbarContentBuilder
    private var filterMenu: some ToolbarContent {
        ToolbarItem {
            Menu {
                Picker("Type", selection: $model.mediaType) {
                    Text("All").tag(MediaTypeFilter?.none)
                    Text("Films").tag(MediaTypeFilter?.some(.movie))
                    Text("Shows").tag(MediaTypeFilter?.some(.show))
                }
                Picker("Genre", selection: $model.genre) {
                    Text("All genres").tag(String?.none)
                    ForEach(model.genres, id: \.self) { genre in
                        Text(genre).tag(String?.some(genre))
                    }
                }
                Picker("Sort by", selection: $model.sortBy) {
                    Text("Title").tag(MediaSortField.title)
                    Text("Year").tag(MediaSortField.year)
                    Text("Rating").tag(MediaSortField.rating)
                    Text("Recently added").tag(MediaSortField.dateAdded)
                    Text("Runtime").tag(MediaSortField.runtime)
                }
                Picker("Order", selection: $model.sortOrder) {
                    Text("Ascending").tag(BeamSortOrder.ascending)
                    Text("Descending").tag(BeamSortOrder.descending)
                }
            } label: {
                Label("Filter", systemImage: "line.3.horizontal.decrease")
            }
        }
    }
}
