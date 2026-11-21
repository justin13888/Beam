// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.content.Context
import androidx.core.net.toUri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.cache.Cache
import androidx.media3.datasource.cache.NoOpCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.media3.exoplayer.offline.DownloadRequest
import androidx.media3.exoplayer.scheduler.Requirements
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.http.beamDataSourceFactory
import dev.beam.android.core.model.DownloadRecord
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.withContext
import okhttp3.Call
import java.io.File
import java.util.concurrent.Executors

/**
 * Offline downloads, on top of Media3's own download stack.
 *
 * Media3's [DownloadManager] is used rather than a hand-rolled fetcher for one
 * decisive reason: the bytes it writes land in a [Cache] that ExoPlayer can
 * read directly. A downloader that wrote plain files would leave playback
 * unable to use them without a second code path, and partial downloads
 * unplayable at all.
 *
 * Range-resume, retry with backoff, and the constraint handling that pauses on
 * a metered network come with it. Reimplementing those is exactly the kind of
 * thing that looks finished and then loses a viewer's 8 GB download to a
 * tunnel.
 */
public class BeamDownloadManager internal constructor(
    private val manager: DownloadManager,
    private val commands: DownloadCommands,
    private val titles: DownloadTitleStore,
    private val artwork: DownloadArtwork,
) {
    /** The underlying manager, for the foreground service that drives it. */
    internal val raw: DownloadManager get() = manager

    /**
     * Every download, finished and failed ones included, re-emitted whenever
     * any of them changes.
     *
     * The manager's own `currentDownloads` leaves out a download in a terminal
     * state, so a list built from it alone drops a download the moment it
     * completes -- the one the viewer most wants to find. Each change is read
     * from the download index too, which keeps every download until it is
     * removed; see [listed].
     *
     * `currentDownloads` is taken on the thread the manager reports on, where
     * Media3 keeps it; the index is read on [Dispatchers.IO], because each read
     * is a SQLite query the index documents must not run on the main thread.
     * Conflated, so a burst of changes reads the index once for the latest.
     */
    public val downloads: Flow<List<DownloadRecord>> =
        callbackFlow {
            fun publish() {
                trySend(manager.currentDownloads)
            }

            val listener =
                object : DownloadManager.Listener {
                    override fun onDownloadChanged(
                        downloadManager: DownloadManager,
                        download: Download,
                        finalException: Exception?,
                    ) = publish()

                    override fun onDownloadRemoved(
                        downloadManager: DownloadManager,
                        download: Download,
                    ) = publish()

                    override fun onIdle(downloadManager: DownloadManager) = publish()
                }

            manager.addListener(listener)
            publish()
            awaitClose { manager.removeListener(listener) }
        }.conflate()
            .map { current ->
                val index = manager.downloadIndex
                withContext(Dispatchers.IO) {
                    index.listed(current).map { it.toRecord(titles, artwork) }
                }
            }

    /**
     * Queue a file for offline playback.
     *
     * The display fields are stored alongside, because the downloads screen has
     * to render with no network at all -- resolving a title over the network to
     * show an offline download would defeat the feature. The poster's bytes are
     * fetched now for the same reason: a URL alone renders as a placeholder
     * once the network is gone.
     *
     * The poster fetch is awaited but cannot fail the enqueue; it runs after
     * the download is queued, so a slow poster never delays the bytes.
     */
    public suspend fun enqueue(
        fileId: String,
        serverId: String,
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
        repository: PlaybackRepository,
    ) {
        val config = repository.playbackConfig(fileId)
        titles.put(
            DownloadTitle(
                fileId = fileId,
                serverId = serverId,
                mediaId = mediaId,
                title = title,
                subtitle = subtitle,
                posterUrl = posterUrl,
            ),
        )
        val request = DownloadRequest.Builder(fileId, config.url.toUri()).build()
        commands.add(request)
        if (posterUrl != null) {
            artwork.pin(posterUrl)
            // The fetch suspends on the network, so the download may have been
            // removed while it was in flight -- and that removal found nothing
            // to evict yet. Checking again once the bytes have landed is what
            // keeps a removed download from leaving its poster behind.
            releaseIfUnused(posterUrl)
        }
    }

    /** Stop a download, keeping the bytes already fetched. */
    public fun pause(fileId: String) {
        commands.setStopReason(fileId, STOP_REASON_USER)
    }

    /** Resume a paused download from where it stopped. */
    public fun resume(fileId: String) {
        commands.setStopReason(fileId, Download.STOP_REASON_NONE)
    }

    /** Delete a download, its bytes, and the poster kept for it. */
    public suspend fun remove(fileId: String) {
        val posterUrl = titles.get(fileId)?.posterUrl
        commands.remove(fileId)
        titles.remove(fileId)
        if (posterUrl != null) {
            releaseIfUnused(posterUrl)
        }
    }

    /**
     * Delete every download that came from [serverId], with its bytes, its
     * title and the posters only it kept.
     *
     * A download belongs to the server it was fetched from; once that server
     * is forgotten its downloads can no longer be resumed, re-authenticated or
     * reported against, and the session that fetched them is gone with it.
     * Downloads from any other server are left alone.
     */
    public suspend fun removeServer(serverId: String) {
        titles
            .all()
            .filter { title -> title.serverId == serverId }
            .forEach { title -> remove(title.fileId) }
    }

    /** Delete every download, and every poster kept for one. */
    public fun removeAll() {
        commands.removeAll()
        // The titles go too. They are what decides whether a poster is still
        // needed, so one left behind here would keep a later download's
        // shared poster pinned after that download was removed as well.
        val removed = titles.all()
        removed.forEach { title -> titles.remove(title.fileId) }
        removed
            .mapNotNull(DownloadTitle::posterUrl)
            .distinct()
            .forEach(artwork::unpin)
    }

    /**
     * Stop keeping [posterUrl] unless another download still shows it.
     *
     * Episodes of one series share a poster, so the poster outlives any one
     * of them and is released with the last.
     */
    private fun releaseIfUnused(posterUrl: String) {
        if (titles.all().none { it.posterUrl == posterUrl }) {
            artwork.unpin(posterUrl)
        }
    }

    /**
     * Only download over an unmetered connection.
     *
     * Defaults to on in [MediaDownloadRepository], which applies it to a
     * manager as it is built, and the viewer's stored cellular preference,
     * which that repository follows, turns it off. A media file is large enough that
     * getting this wrong costs the viewer real money, so the safe default is
     * the one that cannot.
     */
    public fun setRequiresUnmeteredNetwork(required: Boolean) {
        val flags = if (required) Requirements.NETWORK_UNMETERED else Requirements.NETWORK
        manager.requirements = Requirements(flags)
    }

    internal companion object {
        /**
         * Media3 reserves 0 for "not stopped", so a user-initiated pause needs
         * a non-zero reason to be distinguishable from a download that simply
         * has not started.
         */
        const val STOP_REASON_USER: Int = 1

        /**
         * One thread. Downloads are IO-bound against a single server, so
         * parallelism buys nothing and costs seek thrash on the server's disk
         * -- which for a self-hosted box is often a spinning one.
         */
        const val PARALLEL_DOWNLOADS: Int = 1

        /**
         * Build the manager over [calls], which must resolve the server's
         * credential per request.
         *
         * No credential is taken here. The manager lives as long as the
         * process, and a download as long as the viewer keeps it, across
         * sign-outs, re-sign-ins and server switches; a credential captured at
         * construction would be stale after the first of those, and with none
         * to capture -- signed out, offline -- the manager could not be built
         * at all, leaving the downloads list empty exactly when it is needed.
         */
        fun create(
            context: Context,
            calls: Call.Factory,
            commands: DownloadCommands,
            cache: Cache,
            titles: DownloadTitleStore,
            artwork: DownloadArtwork,
        ): BeamDownloadManager {
            val manager =
                DownloadManager(
                    context,
                    StandaloneDatabaseProvider(context),
                    cache,
                    beamDataSourceFactory(context, calls),
                    Executors.newFixedThreadPool(PARALLEL_DOWNLOADS),
                ).apply {
                    maxParallelDownloads = PARALLEL_DOWNLOADS
                }
            return BeamDownloadManager(manager, commands, titles, artwork)
        }

        /**
         * The shared download cache.
         *
         * [NoOpCacheEvictor] on purpose: a download is something the viewer
         * explicitly asked to keep, so evicting it under cache pressure would
         * silently delete content they chose to have offline. Space is
         * reclaimed by deleting downloads, which is the viewer's decision.
         */
        fun cache(context: Context): Cache =
            SimpleCache(
                File(context.filesDir, "downloads"),
                NoOpCacheEvictor(),
                StandaloneDatabaseProvider(context),
            )
    }
}
