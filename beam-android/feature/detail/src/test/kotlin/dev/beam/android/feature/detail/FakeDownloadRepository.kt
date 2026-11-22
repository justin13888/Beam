package dev.beam.android.feature.detail

import dev.beam.android.core.media.download.DownloadRepository
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf
import uniffi.beam_client_core.DownloadRecord

/**
 * A download repository that records what it was asked to fetch.
 *
 * Local to this module for the same reason as the downloads screen's copy:
 * the interface lives in `:core:media`, and a shared fake would put Media3 on
 * every feature's test classpath.
 */
internal class FakeDownloadRepository : DownloadRepository {
    /** Every enqueued file, as (fileId, title). */
    val enqueued: MutableList<Pair<String, String>> = mutableListOf()

    override val downloads: Flow<List<DownloadRecord>> = flowOf(emptyList())

    override suspend fun enqueue(
        fileId: String,
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
        episodeId: String?,
    ) {
        enqueued += fileId to title
    }

    override suspend fun pause(fileId: String) = Unit

    override suspend fun resume(fileId: String) = Unit

    override suspend fun remove(fileId: String) = Unit

    override suspend fun removeAll() = Unit

    override suspend fun removeServer(serverId: String) = Unit

    override suspend fun setRequiresUnmeteredNetwork(required: Boolean) = Unit

    /** The files whose downloads have completed; any other download is still partial. */
    val completed: MutableSet<String> = mutableSetOf()

    /** Every file [isDownloaded] was asked about. */
    val asked: MutableSet<String> = mutableSetOf()

    override suspend fun isDownloaded(fileId: String): Boolean {
        asked += fileId
        return fileId in completed
    }
}
