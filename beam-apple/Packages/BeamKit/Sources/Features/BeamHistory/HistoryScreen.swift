import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// The watch history.
public struct HistoryScreen: View {
    @State private var model: HistoryModel
    private let onOpenTitle: (String) -> Void
    private let onResume: (PlaybackRequest) -> Void

    /// Build the screen over a model.
    public init(
        model: HistoryModel,
        onOpenTitle: @escaping (String) -> Void,
        onResume: @escaping (PlaybackRequest) -> Void
    ) {
        _model = State(wrappedValue: model)
        self.onOpenTitle = onOpenTitle
        self.onResume = onResume
    }

    public var body: some View {
        content
            .navigationTitle("History")
            .task { await model.load() }
            .refreshable { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.entries {
        case .idle, .loading:
            BeamStateView(.loading)
        case .failed(let message):
            BeamStateView(.failed(message: message, isRetryable: true)) {
                Task { await model.load() }
            }
        case .loaded(let entries) where entries.isEmpty:
            BeamStateView(
                .empty(
                    title: "Nothing watched yet",
                    message: "Titles you play appear here.",
                    systemImage: "clock.arrow.circlepath"
                )
            )
        case .loaded(let entries):
            List {
                ForEach(entries, id: \.fileId) { entry in
                    Button {
                        onResume(entry.playbackRequest)
                    } label: {
                        HistoryRow(entry: entry)
                    }
                    .buttonStyle(.plain)
                    .contextMenu {
                        Button("Go to title") { onOpenTitle(entry.mediaId) }
                    }
                    .onAppear {
                        if entry.fileId == entries.last?.fileId {
                            Task { await model.loadMore() }
                        }
                    }
                }
                if model.isLoadingMore {
                    ProgressView().frame(maxWidth: .infinity)
                }
            }
            .listStyle(.plain)
            .beamScrollEdges()
        }
    }
}

/// One watched title.
struct HistoryRow: View {
    let entry: HistoryEntry

    var body: some View {
        HStack(spacing: BeamTheme.Spacing.compact) {
            BeamArtwork(urlString: entry.artworkURL).frame(width: 48)

            VStack(alignment: .leading, spacing: BeamTheme.Spacing.tight) {
                Text(entry.displayTitle).font(.headline).lineLimit(1)
                if let subtitle = entry.displaySubtitle {
                    Text(subtitle)
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                        .lineLimit(1)
                }
                if entry.completed {
                    BeamBadge("Watched", systemImage: "checkmark", emphasis: .positive)
                } else {
                    ProgressView(value: entry.fraction)
                        .progressViewStyle(.linear)
                        .tint(BeamTheme.Colors.accent)
                }
            }
            Spacer()
        }
        .padding(.vertical, BeamTheme.Spacing.tight)
        .accessibilityElement(children: .combine)
    }
}
