package dev.beam.android.feature.downloads

import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.testing.downloadRecord
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState

/**
 * Downloads held in memory, as the core would hand them over.
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
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
        episodeId: String?,
    ) {
        failWith?.let { throw it }
        state.value = state.value +
            downloadRecord(fileId = fileId, title = title, state = DownloadState.QUEUED, episodeId = episodeId)
    }

    override suspend fun pause(fileId: String) {
        failWith?.let { throw it }
        transition(fileId, DownloadState.PAUSED)
    }

    override suspend fun resume(fileId: String) {
        failWith?.let { throw it }
        transition(fileId, DownloadState.DOWNLOADING)
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

    override suspend fun removeServer(serverId: String) {
        failWith?.let { throw it }
        state.value = state.value.filterNot { it.serverId == serverId }
    }

    override suspend fun setRequiresUnmeteredNetwork(required: Boolean) {
        failWith?.let { throw it }
    }

    override suspend fun isDownloaded(fileId: String): Boolean =
        state.value.any { it.fileId == fileId && it.state == DownloadState.COMPLETED }

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
