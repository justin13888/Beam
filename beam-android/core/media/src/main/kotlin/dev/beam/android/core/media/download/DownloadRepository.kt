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
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.model.DownloadRecord
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
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
 *
 * The network requirement follows the stored cellular preference from the
 * moment this is constructed, rather than waiting for a screen to send it:
 * the system can start the process for the download service alone, and the
 * viewer's choice must hold then too.
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
        private val artwork: DownloadArtwork,
        preferences: PreferencesRepository,
    ) : DownloadRepository,
        DownloadManagerHolder {
        private var delegate: BeamDownloadManager? = null

        /**
         * Unmetered only until told otherwise: the stored preference defaults
         * to no downloads over cellular, and a manager built before the stored
         * value has been read must not spend the viewer's data meanwhile.
         */
        private var requiresUnmetered: Boolean = true

        init {
            // Process-lived, like this singleton. The main thread, because a
            // Media3 DownloadManager is driven from the thread it was built on,
            // and the download service builds it there.
            CoroutineScope(SupervisorJob() + Dispatchers.Main).launch {
                followCellularPreference(preferences, this@MediaDownloadRepository)
            }
        }

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

        /**
         * Held here rather than handed straight to the manager, because the
         * stored preference is read at construction -- possibly before
         * sign-in, when building the manager would bake in an empty
         * credential. The manager takes it when it is built, and every change
         * after that.
         */
        override suspend fun setRequiresUnmeteredNetwork(required: Boolean) {
            synchronized(this) {
                requiresUnmetered = required
                delegate?.setRequiresUnmeteredNetwork(required)
            }
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
                        artwork = artwork,
                        server = client.serverHttpConfig(),
                    ).also {
                        it.setRequiresUnmeteredNetwork(requiresUnmetered)
                        delegate = it
                    }
            }
    }

/**
 * Apply the stored cellular preference to [downloads]: the value stored now,
 * then each change of it. An unrelated preference change resends nothing.
 *
 * Suspends for as long as [preferences] emits.
 */
internal suspend fun followCellularPreference(
    preferences: PreferencesRepository,
    downloads: DownloadRepository,
) {
    preferences.preferences
        .map { it.downloadOverCellular }
        .distinctUntilChanged()
        .collect { overCellular -> downloads.setRequiresUnmeteredNetwork(!overCellular) }
}
