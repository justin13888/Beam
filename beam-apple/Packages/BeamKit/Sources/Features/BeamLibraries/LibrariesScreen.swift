import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// The list of libraries.
public struct LibrariesScreen: View {
    @State private var model: LibrariesModel
    private let onOpenLibrary: (LibrarySummary) -> Void

    /// Build the screen over a model.
    public init(model: LibrariesModel, onOpenLibrary: @escaping (LibrarySummary) -> Void) {
        _model = State(wrappedValue: model)
        self.onOpenLibrary = onOpenLibrary
    }

    public var body: some View {
        content
            .navigationTitle("Libraries")
            .task { await model.load() }
            .refreshable { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.libraries {
        case .idle, .loading:
            BeamStateView(.loading)
        case .failed(let message):
            BeamStateView(.failed(message: message, isRetryable: true)) {
                Task { await model.load() }
            }
        case .loaded(let libraries) where libraries.isEmpty:
            BeamStateView(
                .empty(
                    title: "No libraries",
                    message: "An administrator adds libraries from the admin screen.",
                    systemImage: "folder"
                )
            )
        case .loaded(let libraries):
            List(libraries, id: \.id) { library in
                Button {
                    onOpenLibrary(library)
                } label: {
                    LibraryRow(library: library)
                }
                .buttonStyle(.plain)
            }
            .listStyle(.plain)
            .beamScrollEdges()
        }
    }
}

/// One library in the list.
struct LibraryRow: View {
    let library: LibrarySummary

    var body: some View {
        HStack(spacing: BeamTheme.Spacing.compact) {
            Image(systemName: "film.stack")
                .font(.title2)
                .foregroundStyle(BeamTheme.Colors.accent)
                .frame(width: 44)

            VStack(alignment: .leading, spacing: BeamTheme.Spacing.tight) {
                Text(library.name).font(.headline)
                Text("\(library.size) titles")
                    .font(.caption)
                    .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                if let description = library.description, !description.isEmpty {
                    Text(description)
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                        .lineLimit(1)
                }
            }

            Spacer()
            Image(systemName: "chevron.right")
                .font(.footnote)
                .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
        }
        .padding(.vertical, BeamTheme.Spacing.tight)
        .accessibilityElement(children: .combine)
    }
}
