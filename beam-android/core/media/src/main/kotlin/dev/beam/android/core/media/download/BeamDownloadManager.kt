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
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.beamDataSourceFactory
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadReport
import uniffi.beam_client_core.DownloadState
import uniffi.beam_client_core.ServerHttpConfig
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
    private val client: BeamClient,
) {
    /** The underlying manager, for the foreground service that drives it. */
    internal val raw: DownloadManager get() = manager

    /**
     * Every download, re-emitted whenever any of them changes.
     *
     * Media3 is authoritative for state and byte counts -- it owns the
     * transfer and its own persistent index -- so each callback pushes what it
     * reports into the core catalogue, and the catalogue is what is emitted.
     * Reading back rather than mapping in place is what makes this screen and
     * the Apple one render the same records: the display fields live only in
     * the core, and merging them here would be the second implementation this
     * module exists to remove.
     */
    public val downloads: Flow<List<DownloadRecord>> =
        callbackFlow {
            // The listener is an anonymous object, so `this` inside its methods
            // is the listener rather than the producer scope. Naming the scope
            // is what lets a callback start the suspending publish.
            val scope = this

            suspend fun publish() {
                // One call, not two per download: this runs on every progress
                // tick, and a per-file method would reload and rewrite the
                // whole catalogue twice for each download in the list.
                val reports =
                    manager.currentDownloads.map { download ->
                        DownloadReport(
                            fileId = download.request.id,
                            downloadedBytes = download.bytesDownloaded.toULong(),
                            // Media3 reports a negative length until the server
                            // has answered with a Content-Length. `null` means
                            // "not yet known", which the core will not unlearn.
                            totalBytes = download.contentLength.takeIf { it >= 0L }?.toULong(),
                            state = download.state.toDownloadState(),
                        )
                    }
                trySend(client.applyDownloadReports(reports))
            }

            val listener =
                object : DownloadManager.Listener {
                    override fun onDownloadChanged(
                        downloadManager: DownloadManager,
                        download: Download,
                        finalException: Exception?,
                    ) {
                        scope.launch { publish() }
                    }

                    override fun onDownloadRemoved(
                        downloadManager: DownloadManager,
                        download: Download,
                    ) {
                        scope.launch { publish() }
                    }

                    override fun onIdle(downloadManager: DownloadManager) {
                        scope.launch { publish() }
                    }
                }

            manager.addListener(listener)
            publish()
            awaitClose { manager.removeListener(listener) }
        }

    /**
     * Queue a file for offline playback.
     *
     * The display fields are stored alongside, because the downloads screen has
     * to render with no network at all -- resolving a title over the network to
     * show an offline download would defeat the feature.
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
        client.recordDownloadQueued(
            DownloadRecord(
                fileId = fileId,
                serverId = serverId,
                mediaId = mediaId,
                episodeId = null,
                title = title,
                subtitle = subtitle,
                posterUrl = posterUrl,
                fileName = config.url.toUri().lastPathSegment,
                state = DownloadState.Queued,
                downloadedBytes = 0uL,
                totalBytes = null,
            ),
        )
        val request = DownloadRequest.Builder(fileId, config.url.toUri()).build()
        manager.addDownload(request)
    }

    /** Stop a download, keeping the bytes already fetched. */
    public fun pause(fileId: String) {
        manager.setStopReason(fileId, STOP_REASON_USER)
    }

    /** Resume a paused download from where it stopped. */
    public fun resume(fileId: String) {
        manager.setStopReason(fileId, Download.STOP_REASON_NONE)
    }

    /** Delete a download and its bytes. */
    public suspend fun remove(fileId: String) {
        manager.removeDownload(fileId)
        client.forgetDownload(fileId)
    }

    /** Delete every download. */
    public suspend fun removeAll() {
        manager.removeAllDownloads()
        client.forgetAllDownloads()
    }

    /**
     * Only download over an unmetered connection.
     *
     * Defaults to on elsewhere in the app. A media file is large enough that
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

        fun create(
            context: Context,
            clients: BeamHttpClientFactory,
            cache: Cache,
            client: BeamClient,
            server: ServerHttpConfig,
        ): BeamDownloadManager {
            val manager =
                DownloadManager(
                    context,
                    StandaloneDatabaseProvider(context),
                    cache,
                    beamDataSourceFactory(
                        context,
                        clients,
                        server.headers,
                        server.trustedFingerprints,
                    ),
                    Executors.newFixedThreadPool(PARALLEL_DOWNLOADS),
                ).apply {
                    maxParallelDownloads = PARALLEL_DOWNLOADS
                }
            return BeamDownloadManager(manager, client)
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

/**
 * Media3's download state, in the core's vocabulary.
 *
 * `STATE_REMOVING` maps to `Paused` rather than to a state of its own: it is a
 * transient step the viewer never needs to act on, and the record is about to
 * disappear anyway.
 */
private fun Int.toDownloadState(): DownloadState =
    when (this) {
        Download.STATE_QUEUED -> {
            DownloadState.Queued
        }

        Download.STATE_DOWNLOADING -> {
            DownloadState.Downloading
        }

        Download.STATE_COMPLETED -> {
            DownloadState.Completed
        }

        Download.STATE_FAILED -> {
            DownloadState.Failed("The download stopped. It will resume when you retry it.")
        }

        Download.STATE_STOPPED -> {
            DownloadState.Paused
        }

        Download.STATE_RESTARTING -> {
            DownloadState.Downloading
        }

        Download.STATE_REMOVING -> {
            DownloadState.Paused
        }

        else -> {
            DownloadState.Queued
        }
    }
