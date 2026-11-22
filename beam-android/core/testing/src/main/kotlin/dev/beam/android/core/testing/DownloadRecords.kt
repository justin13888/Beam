package dev.beam.android.core.testing

import uniffi.beam_client_core.DownloadPart
import uniffi.beam_client_core.DownloadPlan
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState

/**
 * A download record as the core would hand it over, for a screen test that
 * needs one in a particular state.
 *
 * Only what a test names is set; the rest is a one-part plan from
 * [serverId], so a record built here has the shape the core always gives.
 * Its progress is taken as given rather than derived: deriving it is the
 * core's, and its own tests cover that.
 */
public fun downloadRecord(
    fileId: String = "file-1",
    mediaId: String = "media-1",
    serverId: String = "server-1",
    title: String = "Arrival",
    state: DownloadState = DownloadState.DOWNLOADING,
    downloadedBytes: ULong = 0uL,
    totalBytes: ULong? = null,
    progress: Float? = null,
    episodeId: String? = null,
    artworkPath: String? = null,
    failureMessage: String? = null,
): DownloadRecord =
    DownloadRecord(
        fileId = fileId,
        mediaId = mediaId,
        episodeId = episodeId,
        serverId = serverId,
        title = title,
        subtitle = null,
        artworkPath = artworkPath,
        state = state,
        downloadedBytes = downloadedBytes,
        totalBytes = totalBytes,
        progress = progress,
        failureMessage = failureMessage,
        plan =
            DownloadPlan(
                serverId = serverId,
                titleId = episodeId ?: mediaId,
                fileId = fileId,
                episodeId = episodeId,
                parts =
                    listOf(
                        DownloadPart(
                            fileId = fileId,
                            partNumber = null,
                            downloadUrl = "https://beam.test/v1/files/$fileId/download",
                            sizeBytes = totalBytes,
                            durationSecs = null,
                        ),
                    ),
                subtitles = emptyList(),
            ),
    )
