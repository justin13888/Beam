// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.content.Context
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.cache.Cache
import androidx.media3.exoplayer.offline.DownloadManager
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.model.DownloadRecord
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOf
import uniffi.beam_client_core.BeamClient
import javax.inject.Inject
import javax.inject.Singleton

/** Offline downloads, as the downloads screen sees them. */
public interface DownloadRepository {
    /** Every download, re-emitted whenever any of them changes. */
    public val downloads: Flow<List<DownloadRecord>>

    /** Queue a file for offline playback. */
    public suspend fun enqueue(
        fileId: String,
        serverId: String,
        mediaId: String,
        title: String,
        subtitle: String? = null,
        posterUrl: String? = null,
    )

    /** Stop a download, keeping the bytes already fetched. */
    public suspend fun pause(fileId: String)

    /** Resume a paused download from where it stopped. */
    public suspend fun resume(fileId: String)

    /** Delete a download and its bytes. */
    public suspend fun remove(fileId: String)

    /** Delete every download. */
    public suspend fun removeAll()

    /** Only download over an unmetered connection. */
    public suspend fun setRequiresUnmeteredNetwork(required: Boolean)

    /** Whether a file is already downloaded and playable offline. */
    public suspend fun isDownloaded(fileId: String): Boolean
}

/**
 * A [DownloadRepository] whose manager is built on first use.
 *
 * Construction is deferred because a [DownloadManager] needs the session
 * credential, which does not exist until the viewer has signed in. Building it
 * eagerly at app start would either fail or bake in an empty credential, and
 * every download would then 401 with no obvious cause.
 */
@Singleton
internal class MediaDownloadRepository
    @Inject
    constructor(
        @ApplicationContext private val context: Context,
        private val client: BeamClient,
        private val playback: PlaybackRepository,
        private val clients: BeamHttpClientFactory,
        private val cache: Cache,
        private val titles: DownloadTitleStore,
    ) : DownloadRepository,
        DownloadManagerHolder {
        private var delegate: BeamDownloadManager? = null

        @OptIn(ExperimentalCoroutinesApi::class)
        override val downloads: Flow<List<DownloadRecord>> =
            flow { emit(runCatching { beamManager() }.getOrNull()) }
                .flatMapLatest { built -> built?.downloads ?: flowOf(emptyList()) }

        override suspend fun enqueue(
            fileId: String,
            serverId: String,
            mediaId: String,
            title: String,
            subtitle: String?,
            posterUrl: String?,
        ) {
            beamManager().enqueue(fileId, serverId, mediaId, title, subtitle, posterUrl, playback)
        }

        override suspend fun pause(fileId: String) {
            beamManager().pause(fileId)
        }

        override suspend fun resume(fileId: String) {
            beamManager().resume(fileId)
        }

        override suspend fun remove(fileId: String) {
            beamManager().remove(fileId)
        }

        override suspend fun removeAll() {
            beamManager().removeAll()
        }

        override suspend fun setRequiresUnmeteredNetwork(required: Boolean) {
            beamManager().setRequiresUnmeteredNetwork(required)
        }

        override suspend fun isDownloaded(fileId: String): Boolean =
            titles.get(fileId) != null && cache.getCachedSpans(fileId).isNotEmpty()

        /**
         * The raw manager the download service drives.
         *
         * The service is started by the system, which may happen before any screen
         * has touched the repository, so this builds the manager rather than
         * assuming one exists.
         */
        override fun manager(): DownloadManager = beamManager().raw

        private fun beamManager(): BeamDownloadManager =
            synchronized(this) {
                delegate ?: BeamDownloadManager
                    .create(
                        context = context,
                        clients = clients,
                        cache = cache,
                        titles = titles,
                        server = client.serverHttpConfig(),
                    ).also { delegate = it }
            }
    }
