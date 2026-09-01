package dev.beam.android.feature.downloads

import dev.beam.android.core.media.download.DownloadRepository
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState

/**
 * Downloads held in memory.
 *
 * Local to this module rather than in `:core:testing`: the real repository
 * lives in `:core:media`, and putting its fake in the shared testing module
 * would drag Media3 onto the test classpath of every feature, including the
 * ones that never play anything.
 */
internal class FakeDownloadRepository(
    initial: List<DownloadRecord> = emptyList(),
) : DownloadRepository {
    private val state = MutableStateFlow(initial)

    override val downloads: Flow<List<DownloadRecord>> = state

    /** Set to make every call throw. */
    var failWith: Throwable? = null

    /** Whether [removeAll] was called. */
    var removedEverything: Boolean = false
        private set

    override suspend fun enqueue(
        fileId: String,
        serverId: String,
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
    ) {
        failWith?.let { throw it }
        state.value = state.value +
            DownloadRecord(
                fileId = fileId,
                mediaId = mediaId,
                serverId = serverId,
                title = title,
                subtitle = subtitle,
                posterUrl = posterUrl,
                episodeId = null,
                fileName = null,
                state = DownloadState.Queued,
                downloadedBytes = 0uL,
                totalBytes = null,
            )
    }

    override suspend fun pause(fileId: String) {
        failWith?.let { throw it }
        transition(fileId, DownloadState.Paused)
    }

    override suspend fun resume(fileId: String) {
        failWith?.let { throw it }
        transition(fileId, DownloadState.Downloading)
    }

    override suspend fun remove(fileId: String) {
        failWith?.let { throw it }
        state.value = state.value.filterNot { it.fileId == fileId }
    }

    override suspend fun removeAll() {
        failWith?.let { throw it }
        removedEverything = true
        state.value = emptyList()
    }

    override suspend fun setRequiresUnmeteredNetwork(required: Boolean) {
        failWith?.let { throw it }
    }

    override suspend fun isDownloaded(fileId: String): Boolean =
        state.value.any { it.fileId == fileId && it.state == DownloadState.Completed }

    private fun transition(
        fileId: String,
        to: DownloadState,
    ) {
        state.value =
            state.value.map {
                if (it.fileId == fileId) it.copy(state = to) else it
            }
    }
}

/** A download record, for tests that need one in a particular state. */
internal fun downloadRecord(
    fileId: String = "file-1",
    title: String = "Arrival",
    state: DownloadState = DownloadState.Downloading,
    downloadedBytes: ULong = 0uL,
    totalBytes: ULong? = null,
) = DownloadRecord(
    fileId = fileId,
    serverId = "server-1",
    mediaId = "media-1",
    episodeId = null,
    title = title,
    subtitle = null,
    posterUrl = null,
    fileName = null,
    state = state,
    downloadedBytes = downloadedBytes,
    totalBytes = totalBytes,
)
