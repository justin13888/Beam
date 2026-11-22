package dev.beam.android.feature.detail

import dev.beam.android.core.media.download.DownloadRepository
import uniffi.beam_client_core.SourceSelection

/** Whether Play plays a completed download, from disk rather than the server. */
public val DetailUiState.playsOffline: Boolean
    get() = selection?.source?.fileId?.let(downloadedFileIds::contains) == true

/**
 * The chosen file, where it is downloaded in full, as the download index
 * records it. A download still in progress or paused is not, however much of
 * the file it holds: it would stop playing offline where its bytes run out.
 *
 * Only the chosen file is looked up, because [playsOffline] reads only it;
 * each lookup is an index read, and a show's every episode would be one each.
 */
internal suspend fun DownloadRepository.downloadedOf(selection: SourceSelection?): Set<String> =
    setOfNotNull(selection?.source?.fileId?.takeIf { fileId -> isDownloaded(fileId) })
