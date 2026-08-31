package dev.beam.android.feature.detail

import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.model.DownloadRecord
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf

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
        serverId: String,
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
    ) {
        enqueued += fileId to title
    }

    override suspend fun pause(fileId: String) = Unit

    override suspend fun resume(fileId: String) = Unit

    override suspend fun remove(fileId: String) = Unit

    override suspend fun removeAll() = Unit

    override suspend fun setRequiresUnmeteredNetwork(required: Boolean) = Unit

    override suspend fun isDownloaded(fileId: String): Boolean = false
}
