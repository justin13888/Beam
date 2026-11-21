// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadIndex
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import java.io.IOException

/**
 * Every download this index records, in its order, each replaced by its copy
 * in [current] where it has one.
 *
 * [current] is the manager's live view of the downloads still in progress,
 * and holds progress the index is only written with now and then; the index
 * is what still holds a completed or failed download. A download in [current]
 * that the index does not hold yet is listed after the rest.
 *
 * An index that cannot be read lists [current] alone: the downloads still in
 * progress stay on screen rather than the list going blank.
 */
internal fun DownloadIndex.listed(current: List<Download>): List<Download> {
    val indexed: List<Download> =
        try {
            getDownloads().use { cursor ->
                buildList<Download> { while (cursor.moveToNext()) add(cursor.download) }
            }
        } catch (_: IOException) {
            return current
        }
    val live = current.associateBy { it.request.id }
    val indexedIds = indexed.mapTo(HashSet()) { it.request.id }
    return indexed.map { live[it.request.id] ?: it } + current.filterNot { it.request.id in indexedIds }
}

internal fun Download.toRecord(
    titles: DownloadTitleStore,
    artwork: DownloadArtwork,
): DownloadRecord {
    val title = titles.get(request.id)
    return DownloadRecord(
        fileId = request.id,
        mediaId = title?.mediaId.orEmpty(),
        episodeId = title?.episodeId,
        serverId = title?.serverId.orEmpty(),
        title = title?.title ?: request.id,
        subtitle = title?.subtitle,
        // The copy kept at enqueue time where there is one, so the poster
        // renders with no network.
        posterUrl = title?.posterUrl?.let(artwork::displayUrl),
        state =
            when (state) {
                Download.STATE_QUEUED -> DownloadState.Queued
                Download.STATE_DOWNLOADING -> DownloadState.Downloading
                Download.STATE_COMPLETED -> DownloadState.Completed
                Download.STATE_FAILED -> DownloadState.Failed
                Download.STATE_STOPPED -> DownloadState.Paused
                Download.STATE_RESTARTING -> DownloadState.Downloading
                Download.STATE_REMOVING -> DownloadState.Paused
                else -> DownloadState.Queued
            },
        downloadedBytes = bytesDownloaded,
        // Media3 reports a negative length until the server has answered with
        // a Content-Length, and a negative total would render as a nonsense
        // progress bar.
        totalBytes = contentLength.coerceAtLeast(0L),
        failureMessage =
            if (state == Download.STATE_FAILED) {
                "The download stopped. It will resume when you retry it."
            } else {
                null
            },
    )
}
