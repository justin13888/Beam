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
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadIndex
import androidx.media3.exoplayer.offline.DownloadManager
import dagger.hilt.android.qualifiers.ApplicationContext
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.media.http.ServerCallFactory
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.emitAll
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.DownloadRecord
import java.io.IOException
import javax.inject.Inject
import javax.inject.Singleton

/** Offline downloads, as the downloads screen sees them. */
public interface DownloadRepository {
    /** Every download, re-emitted whenever any of them changes. */
    public val downloads: Flow<List<DownloadRecord>>

    /**
     * Queue a file for offline playback, from the active server: the core
     * plans every part and subtitle file of its source.
     */
    public suspend fun enqueue(
        fileId: String,
        mediaId: String,
        title: String,
        subtitle: String? = null,
        posterUrl: String? = null,
        episodeId: String? = null,
    )

    /** Stop a download, keeping the bytes already fetched. */
    public suspend fun pause(fileId: String)

    /** Resume a paused download from where it stopped. */
    public suspend fun resume(fileId: String)

    /** Delete a download and its bytes. */
    public suspend fun remove(fileId: String)

    /** Delete every download. */
    public suspend fun removeAll()

    /** Delete every download that came from [serverId], leaving other servers' alone. */
    public suspend fun removeServer(serverId: String)

    /** Only download over an unmetered connection. */
    public suspend fun setRequiresUnmeteredNetwork(required: Boolean)

    /**
     * Whether a file is downloaded in full, and so plays offline. A download
     * still in progress, paused, or failed is not, whatever it has fetched.
     */
    public suspend fun isDownloaded(fileId: String): Boolean
}

/** A file downloaded in full: what the player needs to play it with no network. */
public data class CompletedDownload(
    val fileId: String,
    /**
     * The URL it was fetched from. The download cache keys its bytes by it,
     * so playing from this URL reads them from disk.
     */
    val uri: String,
)

/**
 * The completed downloads, as the player looks them up.
 *
 * Narrower than [DownloadRepository] because the player needs only this, and
 * a player test then supplies one completed download without a download
 * manager behind it.
 */
public fun interface CompletedDownloads {
    /** [fileId]'s download, where it has completed; `null` otherwise. */
    public suspend fun completed(fileId: String): CompletedDownload?
}

/**
 * A [DownloadRepository] whose manager is built on first use.
 *
 * The manager holds no credential: its requests go out through [calls], which
 * asks the core on each one for the session of the server it goes to. So it can
 * be built signed out -- the downloads list renders offline and signed out
 * alike -- a download queued under one session carries on under the next after
 * a sign-in, and a download from one server keeps that server's session after
 * the viewer switches to another, all without the manager being rebuilt. Every
 * change goes
 * through [commands], which reach [BeamDownloadService], so the transfer runs in
 * a foreground service rather than dying with the app's process.
 *
 * Construction is still deferred to first use, so an app start that never
 * touches downloads does not open the download index.
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
        private val core: BeamClient,
        private val calls: ServerCallFactory,
        private val commands: DownloadCommands,
        private val cache: Cache,
        private val artwork: DownloadArtwork,
        preferences: PreferencesRepository,
    ) : DownloadRepository,
        CompletedDownloads,
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

        override val downloads: Flow<List<DownloadRecord>> =
            flow { emitAll(beamManager().downloads) }

        override suspend fun enqueue(
            fileId: String,
            mediaId: String,
            title: String,
            subtitle: String?,
            posterUrl: String?,
            episodeId: String?,
        ) {
            beamManager().enqueue(fileId, mediaId, episodeId, title, subtitle, posterUrl)
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

        override suspend fun removeServer(serverId: String) {
            beamManager().removeServer(serverId)
        }

        /**
         * Held here rather than handed straight to the manager, because the
         * stored preference is read at construction, and building the manager
         * then would open the download index on every app start. The manager
         * takes it when it is built, and every change after that.
         *
         * Set on the manager directly rather than sent to the service: the
         * preference is followed from app start, when the app may not be in
         * front, and a service start from the background is refused.
         */
        override suspend fun setRequiresUnmeteredNetwork(required: Boolean) {
            synchronized(this) {
                requiresUnmetered = required
                delegate?.setRequiresUnmeteredNetwork(required)
            }
        }

        /**
         * From the download index, as [completed] is, so the detail page says
         * "Play offline" exactly when the player would read the file from disk.
         * The core's record catches up only while something collects
         * [downloads], so it can lag a download that finished meanwhile.
         */
        override suspend fun isDownloaded(fileId: String): Boolean = completed(fileId) != null

        /**
         * From the download index; see [completedDownload].
         *
         * The manager is taken on the caller's thread, which builds it on
         * first use where Media3 needs it built, and the index is read on
         * [Dispatchers.IO]: each read is a SQLite query, which Media3's index
         * documents must not run on the main thread.
         */
        override suspend fun completed(fileId: String): CompletedDownload? {
            val index = beamManager().raw.downloadIndex
            return withContext(Dispatchers.IO) { index.completedDownload(fileId) }
        }

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
                        calls = calls,
                        commands = commands,
                        cache = cache,
                        core = core,
                        artwork = artwork,
                    ).also {
                        it.setRequiresUnmeteredNetwork(requiresUnmetered)
                        delegate = it
                    }
            }
    }

/**
 * [fileId]'s download, where this index records it as completed.
 *
 * The index, not the cache: a partial download has spans in the cache too,
 * and the cache keys them by URL, not by file. An index that cannot be read
 * is treated as holding no completed download, so playback falls back to the
 * core's plan rather than failing.
 */
internal fun DownloadIndex.completedDownload(fileId: String): CompletedDownload? {
    val download =
        try {
            getDownload(fileId)
        } catch (_: IOException) {
            null
        }
    if (download == null || download.state != Download.STATE_COMPLETED) return null
    return CompletedDownload(fileId = fileId, uri = download.request.uri.toString())
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
