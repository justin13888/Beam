import BeamCore
import BeamDesignSystem
import BeamFFI
import BeamModel
import BeamUI
import SwiftUI

/// One title's page.
public struct DetailScreen: View {
    @State private var model: DetailModel
    private let onPlay: (PlaybackRequest) -> Void
    private let onDownload: (String, String) -> Void

    /// Build the screen over a model.
    public init(
        model: DetailModel,
        onPlay: @escaping (PlaybackRequest) -> Void,
        onDownload: @escaping (String, String) -> Void
    ) {
        _model = State(wrappedValue: model)
        self.onPlay = onPlay
        self.onDownload = onDownload
    }

    public var body: some View {
        content
            .navigationTitle(model.summary?.title ?? "")
            #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
            #endif
            .task { await model.load() }
    }

    @ViewBuilder
    private var content: some View {
        switch model.detail {
        case .idle, .loading:
            BeamStateView(.loading)
        case .failed(let message):
            BeamStateView(.failed(message: message, isRetryable: true)) {
                Task { await model.load() }
            }
        case .loaded:
            ScrollView {
                VStack(alignment: .leading, spacing: BeamTheme.Spacing.loose) {
                    hero
                    actions
                    overview
                    if !model.seasons.isEmpty { episodes }
                    if !model.sources.isEmpty {
                        SourcePicker(
                            sources: model.sources,
                            selection: model.selection,
                            chosenFileId: model.chosenFileId,
                            onChoose: model.choose(fileId:)
                        )
                    }
                }
                .padding(BeamTheme.Spacing.regular)
            }
            .beamScrollEdges()
        }
    }

    @ViewBuilder
    private var hero: some View {
        if let summary = model.summary {
            ZStack(alignment: .bottomLeading) {
                BeamArtwork(
                    urlString: summary.backdropUrl ?? summary.posterUrl,
                    aspectRatio: BeamTheme.AspectRatio.backdrop,
                    cornerRadius: BeamTheme.Radius.large
                )
                // The artwork extends under the surrounding chrome rather than
                // stopping at a hard edge, which is what makes the glass above
                // it read as floating over the picture.
                .backgroundExtensionEffect()

                VStack(alignment: .leading, spacing: BeamTheme.Spacing.small) {
                    Text(summary.title)
                        .font(BeamTheme.Typography.screenTitle)
                    HStack(spacing: BeamTheme.Spacing.small) {
                        if let year = summary.year { BeamBadge(String(year)) }
                        if let runtime = summary.runtimeMinutes {
                            BeamBadge(BeamFormat.duration(seconds: Double(runtime) * 60))
                        }
                        if let rating = summary.tmdbRating {
                            BeamBadge("\(rating)%", systemImage: "star.fill", emphasis: .positive)
                        }
                    }
                }
                .padding(BeamTheme.Spacing.regular)
            }
        }
    }

    @ViewBuilder
    private var actions: some View {
        BeamGlassGroup {
            HStack(spacing: BeamTheme.Spacing.compact) {
                Button {
                    if let request = model.playbackRequest() { onPlay(request) }
                } label: {
                    Label("Play", systemImage: "play.fill")
                        .frame(maxWidth: .infinity)
                }
                .buttonStyle(.glassProminent)
                .disabled(!model.hasPlayableSource)

                Button {
                    if let fileId = model.effectiveFileId, let summary = model.summary {
                        onDownload(fileId, summary.title)
                    }
                } label: {
                    Label("Download", systemImage: "arrow.down.circle")
                }
                .buttonStyle(.glass)
                .disabled(model.effectiveFileId == nil)
            }
        }

        if let reason = model.unplayableReason {
            Label(reason, systemImage: "exclamationmark.triangle")
                .font(.footnote)
                .foregroundStyle(BeamTheme.Colors.caution)
        }
    }

    @ViewBuilder
    private var overview: some View {
        if let description = model.summary?.description, !description.isEmpty {
            Text(description).font(.body)
        }
        if let genres = model.summary?.genres, !genres.isEmpty {
            HStack(spacing: BeamTheme.Spacing.small) {
                ForEach(genres, id: \.self) { BeamBadge($0) }
            }
        }
    }

    @ViewBuilder
    private var episodes: some View {
        VStack(alignment: .leading, spacing: BeamTheme.Spacing.compact) {
            Picker("Season", selection: $model.selectedSeason) {
                ForEach(model.seasons, id: \.seasonNumber) { season in
                    Text("Season \(season.seasonNumber)").tag(season.seasonNumber)
                }
            }
            .pickerStyle(.menu)

            ForEach(model.episodesInSelectedSeason, id: \.id) { episode in
                Button {
                    if let request = model.playbackRequest(for: episode) { onPlay(request) }
                } label: {
                    EpisodeRow(episode: episode)
                }
                .buttonStyle(.plain)
                // An episode with no indexed file cannot play. Disabling the
                // row says so before the tap, where a failure afterwards would
                // look like the app losing the file.
                .disabled(episode.fileId == nil)
            }
        }
    }
}

/// One episode in a season.
struct EpisodeRow: View {
    let episode: EpisodeSummary

    var body: some View {
        HStack(spacing: BeamTheme.Spacing.compact) {
            BeamArtwork(
                urlString: episode.thumbnailUrl,
                aspectRatio: BeamTheme.AspectRatio.backdrop
            )
            .frame(width: 120)

            VStack(alignment: .leading, spacing: BeamTheme.Spacing.tight) {
                Text("\(episode.episodeNumber). \(episode.title)")
                    .font(.headline)
                    .lineLimit(2)
                if let duration = episode.durationSecs {
                    Text(BeamFormat.duration(seconds: duration))
                        .font(.caption)
                        .foregroundStyle(BeamTheme.Colors.onGlassSecondary)
                }
                if episode.fileId == nil {
                    BeamBadge("Not indexed", emphasis: .unavailable)
                }
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
    }
}
