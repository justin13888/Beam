package dev.beam.android.core.media.download

import android.app.Notification
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.media3.exoplayer.offline.DownloadNotificationHelper
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
