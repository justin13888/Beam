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
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
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
    private val titles: DownloadTitleStore,
) {
    /** The underlying manager, for the foreground service that drives it. */
    internal val raw: DownloadManager get() = manager

    /** Every download, re-emitted whenever any of them changes. */
    public val downloads: Flow<List<DownloadRecord>> =
        callbackFlow {
            fun publish() {
                trySend(manager.currentDownloads.map { it.toRecord(titles) })
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
        titles.remove(fileId)
    }

    /** Delete every download. */
    public fun removeAll() {
        manager.removeAllDownloads()
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
            titles: DownloadTitleStore,
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
            return BeamDownloadManager(manager, titles)
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

private fun Download.toRecord(titles: DownloadTitleStore): DownloadRecord {
    val title = titles.get(request.id)
    return DownloadRecord(
        fileId = request.id,
        mediaId = title?.mediaId.orEmpty(),
        episodeId = title?.episodeId,
        serverId = title?.serverId.orEmpty(),
        title = title?.title ?: request.id,
        subtitle = title?.subtitle,
        posterUrl = title?.posterUrl,
        state =
            when (state) {
                Download.STATE_QUEUED -> DownloadState.Queued
                Download.STATE_DOWNLOADING -> DownloadState.Downloading
                Download.STATE_COMPLETED -> DownloadState.Completed
                Download.STATE_FAILED -> DownloadState.Failed
                Download.STATE_STOPPED -> DownloadState.Paused
                Download.STATE_RESTARTING -> DownloadState.Downloading
                Download.STATE_REMOVING -> DownloadState.Paused
                else -> DownloadState.Queued
            },
        downloadedBytes = bytesDownloaded,
        // Media3 reports a negative length until the server has answered with
        // a Content-Length, and a negative total would render as a nonsense
        // progress bar.
        totalBytes = contentLength.coerceAtLeast(0L),
        failureMessage =
            if (state == Download.STATE_FAILED) {
                "The download stopped. It will resume when you retry it."
            } else {
                null
            },
    )
}
