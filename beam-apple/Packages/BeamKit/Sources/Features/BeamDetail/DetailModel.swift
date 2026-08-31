import BeamCore
import BeamFFI
import BeamModel
import Foundation

/// One title: its metadata, its episodes, and every file behind it.
///
/// Sources are loaded beside the detail rather than on demand, because the
/// detail screen is where a viewer decides *whether* something will play. Under
/// direct play (ADR-0004) that is not a detail hidden behind a menu -- a title
/// whose only file this device cannot decode should say so before the play
/// button is pressed, not after.
@MainActor
@Observable
public final class DetailModel {
    /// The title.
    public private(set) var detail: LoadState<MediaDetail> = .idle
    /// Every file behind it, playable or not.
    public private(set) var sources: [MediaSourceView] = []
    /// The core's pick, and why the rest were rejected.
    public private(set) var selection: SourceSelection?
    /// The file the viewer has chosen, overriding the core's pick.
    public private(set) var chosenFileId: String?
    /// Set when the download action failed, for a transient banner.
    public var actionMessage: String?

    /// Which season is showing, for a show.
    public var selectedSeason: UInt32 = 1

    @ObservationIgnored private let mediaId: String
    @ObservationIgnored private let catalog: any CatalogRepository
    @ObservationIgnored private let playback: any PlaybackRepository
    @ObservationIgnored private let quality: QualityPreference

    /// Build a model for one title.
    public init(
        mediaId: String,
        catalog: any CatalogRepository,
        playback: any PlaybackRepository,
        quality: QualityPreference
    ) {
        self.mediaId = mediaId
        self.catalog = catalog
        self.playback = playback
        self.quality = quality
    }

    /// The file that would play if the viewer pressed play now.
    ///
    /// The viewer's explicit choice wins over the core's, and only falls back
    /// to the core's pick when they have not made one.
    public var effectiveFileId: String? {
        chosenFileId ?? selection?.source.fileId
    }

    /// Whether anything here can play on this device.
    public var hasPlayableSource: Bool {
        selection != nil
    }

    /// Why nothing plays, when nothing does.
    ///
    /// Built from the core's rejection reasons rather than invented, so the
    /// explanation matches the decision that was actually made.
    public var unplayableReason: String? {
        guard selection == nil, !sources.isEmpty else { return nil }
        if let rejection = selection?.rejected.first {
            return rejection.detail
        }
        return "No file behind this title can be played on this device."
    }

    /// Load the title and its sources.
    public func load() async {
        detail = .loading
        async let detailResult = loadDetail()
        async let sourcesResult = loadSources()
        let (loadedDetail, loadedSources) = await (detailResult, sourcesResult)

        detail = loadedDetail
        sources = loadedSources.sources
        selection = loadedSources.selection

        if case .loaded(.show(_, let seasons)) = loadedDetail, let first = seasons.first {
            selectedSeason = first.seasonNumber
        }
    }

    /// Choose a specific file, overriding the core's pick.
    ///
    /// Only a source the core did not reject can be chosen: offering the
    /// choice and then failing to play it would be worse than not offering it,
    /// and the reason is already on screen beside it.
    public func choose(fileId: String) {
        guard sources.contains(where: { $0.fileId == fileId }),
            selection?.rejected.contains(where: { $0.fileId == fileId }) != true
        else {
            return
        }
        chosenFileId = fileId
    }

    /// Build a playback request for the current choice.
    public func playbackRequest(startAt seconds: Double = 0) -> PlaybackRequest? {
        guard let fileId = effectiveFileId, let summary else { return nil }
        return PlaybackRequest(
            fileId: fileId,
            mediaId: mediaId,
            episodeId: nil,
            title: summary.title,
            subtitle: summary.year.map(String.init),
            artworkUrl: summary.backdropUrl ?? summary.posterUrl,
            startPositionSeconds: seconds
        )
    }

    /// A playback request for one episode.
    public func playbackRequest(for episode: EpisodeSummary) -> PlaybackRequest? {
        guard let fileId = episode.fileId, let summary else { return nil }
        return PlaybackRequest(
            fileId: fileId,
            mediaId: mediaId,
            episodeId: episode.id,
            title: summary.title,
            subtitle: "Episode \(episode.episodeNumber) - \(episode.title)",
            artworkUrl: episode.thumbnailUrl ?? summary.posterUrl,
            startPositionSeconds: 0
        )
    }

    /// The title's own summary, whichever kind it is.
    public var summary: MediaSummary? {
        switch detail.value {
        case .movie(let summary): summary
        case .show(let summary, _): summary
        case nil: nil
        }
    }

    /// The seasons, for a show.
    public var seasons: [SeasonSummary] {
        if case .show(_, let seasons) = detail.value { return seasons }
        return []
    }

    /// The episodes in the selected season.
    public var episodesInSelectedSeason: [EpisodeSummary] {
        seasons.first { $0.seasonNumber == selectedSeason }?.episodes ?? []
    }

    private func loadDetail() async -> LoadState<MediaDetail> {
        do {
            return .loaded(try await catalog.detail(mediaId: mediaId))
        } catch {
            return .failed(BeamFailure.from(error).message)
        }
    }

    private func loadSources() async -> (sources: [MediaSourceView], selection: SourceSelection?) {
        let loaded = (try? await playback.sources(mediaId: mediaId)) ?? []
        // A failed selection is not an error to show: it means nothing here
        // plays on this device, which `unplayableReason` explains from the
        // rejections the sources themselves carry.
        let picked = try? await playback.selectSource(mediaId: mediaId, policy: quality.policy)
        return (loaded, picked)
    }
}
