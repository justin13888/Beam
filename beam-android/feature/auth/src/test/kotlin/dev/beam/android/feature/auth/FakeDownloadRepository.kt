package dev.beam.android.feature.auth

import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.testing.downloadRecord
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState

/**
 * Downloads held in memory, each tagged with the server it came from.
 *
 * Local to this module for the same reason as the settings screen's copy:
 * the interface lives in `:core:media`, and a shared fake would put Media3 on
 * every feature's test classpath.
 */
internal class FakeDownloadRepository(
    initial: List<DownloadRecord> = emptyList(),
) : DownloadRepository {
    private val state = MutableStateFlow(initial)

    override val downloads: Flow<List<DownloadRecord>> = state

    /** What is downloaded now. */
    val current: List<DownloadRecord> get() = state.value

    override suspend fun enqueue(
        fileId: String,
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
        episodeId: String?,
    ) {
        state.value = state.value + download(fileId = fileId, serverId = ACTIVE_SERVER)
    }

    override suspend fun pause(fileId: String) = Unit

    override suspend fun resume(fileId: String) = Unit

    override suspend fun remove(fileId: String) {
        state.value = state.value.filterNot { it.fileId == fileId }
    }

    override suspend fun removeAll() {
        state.value = emptyList()
    }

    override suspend fun removeServer(serverId: String) {
        state.value = state.value.filterNot { it.serverId == serverId }
    }

    override suspend fun setRequiresUnmeteredNetwork(required: Boolean) = Unit

    override suspend fun isDownloaded(fileId: String): Boolean = state.value.any { it.fileId == fileId }
}

/** A completed download of [fileId] from [serverId]. */
internal fun download(
    fileId: String,
    serverId: String,
) = downloadRecord(
    fileId = fileId,
    serverId = serverId,
    title = "Title $fileId",
    state = DownloadState.COMPLETED,
)

/** The server the core queues a download on: the active one, whichever that is. */
private const val ACTIVE_SERVER = "active"
