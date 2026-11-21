// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.app.Notification
import android.content.Context
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.media3.exoplayer.offline.DownloadNotificationHelper
import androidx.media3.exoplayer.offline.DownloadRequest
import androidx.media3.exoplayer.offline.DownloadService
import androidx.media3.exoplayer.scheduler.PlatformScheduler
import androidx.media3.exoplayer.scheduler.Scheduler
import dagger.hilt.android.AndroidEntryPoint
import javax.inject.Inject

/**
 * The foreground service that actually moves the bytes.
 *
 * Media3 requires a [DownloadService] subclass; without one, downloads run
 * only while the app is in front and stop the moment it is backgrounded, which
 * for a multi-gigabyte file is the same as not working. The service also
 * carries the notification the platform requires for long-running work.
 */
@AndroidEntryPoint
public class BeamDownloadService :
    DownloadService(
        FOREGROUND_NOTIFICATION_ID,
        DEFAULT_FOREGROUND_NOTIFICATION_UPDATE_INTERVAL,
        CHANNEL_ID,
        androidx.media3.exoplayer.R.string.exo_download_notification_channel_name,
        0,
    ) {
    @Inject
    internal lateinit var downloads: DownloadManagerHolder

    private val notifications: DownloadNotificationHelper by lazy {
        DownloadNotificationHelper(this, CHANNEL_ID)
    }

    override fun getDownloadManager(): DownloadManager = downloads.manager()

    /**
     * A [PlatformScheduler] so a download interrupted by the constraints --
     * losing Wi-Fi, say -- is resumed by the system when they are met again,
     * rather than waiting for the viewer to reopen the app and notice.
     */
    override fun getScheduler(): Scheduler = PlatformScheduler(this, JOB_ID)

    override fun getForegroundNotification(
        downloads: List<Download>,
        notMetRequirements: Int,
    ): Notification =
        notifications.buildProgressNotification(
            // context =
            this,
            // smallIcon =
            android.R.drawable.stat_sys_download,
            // contentIntent =
            null,
            // message =
            null,
            // downloads =
            downloads,
            // notMetRequirements =
            notMetRequirements,
        )

    internal companion object {
        const val CHANNEL_ID: String = "beam-downloads"
        const val FOREGROUND_NOTIFICATION_ID: Int = 2
        const val JOB_ID: Int = 3
    }
}

/** Hands the service the one [DownloadManager] the app owns. */
public interface DownloadManagerHolder {
    /** The shared download manager. */
    public fun manager(): DownloadManager
}

/**
 * The changes a viewer makes to their downloads, as commands to whatever runs
 * them.
 *
 * A seam rather than calls on the [DownloadManager] itself, because the
 * production path is an intent to [BeamDownloadService]: a command sent that
 * way starts the service, which then holds the transfer in the foreground with
 * a progress notification. A change made on the manager directly runs the
 * transfer in the app's process with no service, so it stops when the process
 * does.
 */
internal interface DownloadCommands {
    /** Queue [request], or update the download already queued under its id. */
    fun add(request: DownloadRequest)

    /** Stop a download with [stopReason], or restart it with [Download.STOP_REASON_NONE]. */
    fun setStopReason(
        id: String,
        stopReason: Int,
    )

    /** Delete a download and its bytes. */
    fun remove(id: String)

    /** Delete every download and its bytes. */
    fun removeAll()
}

/**
 * [DownloadCommands] sent to [BeamDownloadService] through Media3's
 * `DownloadService.send*`.
 *
 * Sent with `foreground = false`: every command comes from a screen the viewer
 * is using, so the app is in front and a plain `startService` is allowed, and
 * the service promotes itself to the foreground once a download is running.
 * Asking for a foreground start here would need the platform's
 * foreground-service start permission at the moment of the call, which a
 * pause or a removal does not warrant.
 */
internal class ServiceDownloadCommands(
    private val context: Context,
) : DownloadCommands {
    override fun add(request: DownloadRequest) {
        DownloadService.sendAddDownload(context, BeamDownloadService::class.java, request, FOREGROUND)
    }

    override fun setStopReason(
        id: String,
        stopReason: Int,
    ) {
        DownloadService.sendSetStopReason(context, BeamDownloadService::class.java, id, stopReason, FOREGROUND)
    }

    override fun remove(id: String) {
        DownloadService.sendRemoveDownload(context, BeamDownloadService::class.java, id, FOREGROUND)
    }

    override fun removeAll() {
        DownloadService.sendRemoveAllDownloads(context, BeamDownloadService::class.java, FOREGROUND)
    }

    private companion object {
        const val FOREGROUND: Boolean = false
    }
}
