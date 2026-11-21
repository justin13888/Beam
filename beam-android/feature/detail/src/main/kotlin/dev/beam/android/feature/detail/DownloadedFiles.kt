package dev.beam.android.feature.detail

import dev.beam.android.core.media.download.DownloadRepository
import uniffi.beam_client_core.MediaDetail
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.SourceSelection

/** Whether Play plays a completed download, from disk rather than the server. */
public val DetailUiState.playsOffline: Boolean
    get() = selection?.source?.fileId?.let(downloadedFileIds::contains) == true

/**
 * The files a title's page can play: each of its sources, the one the core
 * chose, and each episode's.
 */
internal fun offeredFileIds(
    detail: MediaDetail,
    sources: List<MediaSourceView>,
    selection: SourceSelection?,
): Set<String> =
    buildSet {
        sources.forEach { source -> add(source.fileId) }
        selection?.source?.fileId?.let(::add)
        (detail as? MediaDetail.Show)?.seasons.orEmpty().forEach { season ->
            season.episodes.forEach { episode -> episode.fileId?.let(::add) }
        }
    }

/**
 * Which of [fileIds] are downloaded in full, as the download index records
 * them. A download still in progress or paused is not, however much of the
 * file it holds: it would stop playing offline where its bytes run out.
 */
internal suspend fun DownloadRepository.completedAmong(fileIds: Set<String>): Set<String> =
    fileIds.filterTo(mutableSetOf()) { fileId -> isDownloaded(fileId) }
