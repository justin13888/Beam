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
import dev.beam.android.core.media.http.beamDataSourceFactory
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.launch
import okhttp3.Call
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.DownloadRecord
import java.io.File
import java.util.concurrent.Executors
import uniffi.beam_client_core.DownloadRequest as CoreDownloadRequest

/**
 * Offline downloads: the core's records, transferred by Media3.
 *
 * The core owns what a download *is* -- its plan, its display fields, its
 * state and its progress -- so this client and the Apple one cannot disagree
 * about any of them. What stays here is the transfer: Media3's
 * [DownloadManager] writes into a [Cache] that ExoPlayer can read directly,
 * and brings range-resume, retry with backoff and the constraint handling
 * that pauses on a metered network. Reimplementing those is exactly the kind
 * of thing that looks finished and then loses a viewer's 8 GB download to a
 * tunnel.
 *
 * Each item of a download's plan -- every part, and every subtitle file
 * beside one -- is one Media3 download, keyed by the item's id, and what
 * Media3 says about it goes back to the core as a report.
 */
public class BeamDownloadManager internal constructor(
    private val manager: DownloadManager,
    private val commands: DownloadCommands,
    private val core: BeamClient,
    private val artwork: DownloadArtwork,
) {
    /** The underlying manager, for the foreground service that drives it. */
    internal val raw: DownloadManager get() = manager

    /**
     * Every download, re-emitted whenever any of them changes.
     *
     * Read from the core's records rather than from Media3's index: the
     * index forgets nothing it was not told, but knows nothing of titles,
     * and its `currentDownloads` leaves out a finished download altogether.
     */
    public val downloads: Flow<List<DownloadRecord>> =
        callbackFlow {
            // Media3 calls back on its own thread and the core is suspending,
            // so the changes queue here and are reported in order.
            val changes = Channel<Pair<List<Download>, Int>>(Channel.UNLIMITED)
            val listener =
                object : DownloadManager.Listener {
                    override fun onDownloadChanged(
                        downloadManager: DownloadManager,
                        download: Download,
                        finalException: Exception?,
                    ) {
                        changes.trySend(listOf(download) to downloadManager.notMetRequirements)
                    }

                    override fun onDownloadRemoved(
                        downloadManager: DownloadManager,
                        download: Download,
                    ) {
                        changes.trySend(emptyList<Download>() to downloadManager.notMetRequirements)
                    }

                    override fun onRequirementsStateChanged(
                        downloadManager: DownloadManager,
                        requirements: Requirements,
                        notMetRequirements: Int,
                    ) {
                        changes.trySend(downloadManager.currentDownloads to notMetRequirements)
                    }
                }

            manager.addListener(listener)
            changes.trySend(manager.currentDownloads to manager.notMetRequirements)
            launch {
                // The requirements are read with the downloads, on Media3's
                // thread, since the manager is not to be read from any other.
                for ((batch, unmet) in changes) {
                    send(core.reportDownloadTransfers(batch.map { it.toReport(unmet) }))
                }
            }
            awaitClose {
                manager.removeListener(listener)
                changes.close()
            }
        }

    /**
     * Queue a file for offline playback.
     *
     * The core plans it and records it with its display fields, because the
     * downloads screen has to render with no network at all. The poster's
     * bytes are fetched now for the same reason, and the record names where
     * they are kept rather than the URL they came from: a URL renders as a
     * placeholder once the network is gone.
     *
     * The poster fetch is awaited but cannot fail the enqueue; it runs after
     * the transfer is queued, so a slow poster never delays the bytes.
     */
    public suspend fun enqueue(
        fileId: String,
        mediaId: String,
        episodeId: String?,
        title: String,
        subtitle: String?,
        posterUrl: String?,
    ) {
        val record =
            core.enqueueDownload(
                CoreDownloadRequest(
                    mediaId = mediaId,
                    episodeId = episodeId,
                    fileId = fileId,
                    title = title,
                    subtitle = subtitle,
                    artworkPath = posterUrl?.let(artwork::pathFor),
                ),
            )
        record.plan.parts.forEach { part ->
            commands.add(DownloadRequest.Builder(part.fileId, part.downloadUrl.toUri()).build())
        }
        record.plan.subtitles.forEach { sidecar ->
            commands.add(DownloadRequest.Builder(sidecar.sidecarId, sidecar.url.toUri()).build())
        }
        if (posterUrl != null) {
            artwork.pin(posterUrl)
            // The fetch suspends on the network, so the download may have been
            // removed while it was in flight -- and that removal found nothing
            // to delete yet. Checking again once the bytes have landed is what
            // keeps a removed download from leaving its poster behind.
            record.artworkPath?.let { releaseIfUnused(it) }
        }
    }

    /** Stop a download, keeping the bytes already fetched. */
    public suspend fun pause(fileId: String) {
        recordOf(fileId)?.itemIds()?.forEach { commands.setStopReason(it, STOP_REASON_USER) }
    }

    /** Resume a paused download from where it stopped. */
    public suspend fun resume(fileId: String) {
        recordOf(fileId)?.itemIds()?.forEach { commands.setStopReason(it, Download.STOP_REASON_NONE) }
    }

    /** Delete a download, the bytes of every item, and the poster kept for it. */
    public suspend fun remove(fileId: String) {
        val removed = core.removeDownload(fileId)
        if (removed == null) {
            // Nothing the core knows of; whatever Media3 holds under the id goes.
            commands.remove(fileId)
            return
        }
        release(listOf(removed))
    }

    /**
     * Delete every download that came from [serverId], with its bytes and
     * the posters only it kept.
     *
     * A download belongs to the server it was fetched from; once that server
     * is forgotten its downloads can no longer be resumed, re-authenticated or
     * reported against. Downloads from any other server are left alone.
     */
    public suspend fun removeServer(serverId: String) {
        release(core.removeServerDownloads(serverId))
    }

    /** Delete every download, and every poster kept for one. */
    public suspend fun removeAll() {
        // The records go first, as in every removal here: the list the screen
        // shows is re-read when Media3 reports the removal, and a record still
        // there then would stay on screen with nothing left to report it gone.
        val removed = core.removeAllDownloads()
        commands.removeAll()
        removed
            .mapNotNull(DownloadRecord::artworkPath)
            .distinct()
            .forEach(artwork::release)
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

    private suspend fun recordOf(fileId: String): DownloadRecord? = core.downloads().firstOrNull { it.fileId == fileId }

    /** Remove every item of [removed] from Media3, then the posters nothing else shows. */
    private suspend fun release(removed: List<DownloadRecord>) {
        removed.flatMap { it.itemIds() }.forEach(commands::remove)
        removed
            .mapNotNull(DownloadRecord::artworkPath)
            .distinct()
            .forEach { releaseIfUnused(it) }
    }

    /**
     * Stop keeping the poster at [path] unless another download still shows
     * it. Episodes of one series share a poster, so the poster outlives any
     * one of them and is released with the last.
     */
    private suspend fun releaseIfUnused(path: String) {
        if (core.downloads().none { it.artworkPath == path }) {
            artwork.release(path)
        }
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
            core: BeamClient,
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
            return BeamDownloadManager(manager, commands, core, artwork)
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
