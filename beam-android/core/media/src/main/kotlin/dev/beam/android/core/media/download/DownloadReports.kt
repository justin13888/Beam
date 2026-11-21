// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadManager
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState
import uniffi.beam_client_core.TransferReport

/**
 * What Media3 says about one item, in the core's terms.
 *
 * The one mapping that cannot live in the core: only this side knows that a
 * stopped download with the user's stop reason is a pause, or that a queued
 * one whose requirements are unmet is waiting for a network the cellular
 * preference allows. What the item's state means for the whole download is
 * the core's to decide.
 *
 * @param notMetRequirements the manager's unmet requirements when [this] was
 *   read; any at all leave a queued item waiting for a network, because the
 *   network is the only requirement the manager is given.
 */
internal fun Download.toReport(notMetRequirements: Int): TransferReport {
    val itemState =
        when (state) {
            Download.STATE_QUEUED -> {
                if (notMetRequirements != 0) DownloadState.WAITING_FOR_NETWORK else DownloadState.QUEUED
            }

            Download.STATE_DOWNLOADING, Download.STATE_RESTARTING -> {
                DownloadState.DOWNLOADING
            }

            Download.STATE_COMPLETED -> {
                DownloadState.COMPLETED
            }

            Download.STATE_FAILED -> {
                DownloadState.FAILED
            }

            Download.STATE_STOPPED, Download.STATE_REMOVING -> {
                DownloadState.PAUSED
            }

            else -> {
                DownloadState.QUEUED
            }
        }
    return TransferReport(
        itemId = request.id,
        state = itemState,
        downloadedBytes = bytesDownloaded.coerceAtLeast(0L).toULong(),
        // Media3 reports a negative length until the server has answered with
        // a Content-Length; the core reads `null` as not yet known.
        totalBytes = contentLength.takeIf { it > 0L }?.toULong(),
        failureMessage =
            if (itemState == DownloadState.FAILED) {
                "The download stopped. It will resume when you retry it."
            } else {
                null
            },
    )
}

/**
 * Every download in the manager's index, in every state: unlike
 * `currentDownloads`, which leaves out a completed or failed one. Reads the
 * index's database, so it is called off the main thread.
 */
internal fun DownloadManager.indexedDownloads(): List<Download> =
    downloadIndex.getDownloads().use { cursor ->
        buildList { while (cursor.moveToNext()) add(cursor.download) }
    }

/** The id of every Media3 download behind [this]: each part, then each subtitle file. */
internal fun DownloadRecord.itemIds(): List<String> = plan.parts.map { it.fileId } + plan.subtitles.map { it.sidecarId }
