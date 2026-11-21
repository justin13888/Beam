// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.media3.exoplayer.offline.DownloadRequest

/**
 * [DownloadCommands] applied to [manager] in this process, which is what
 * [BeamDownloadService] does with each command once it has started.
 *
 * Robolectric records a service start without running the service, so a test
 * that needs a command to reach the manager stands this in for the intent.
 * That the production commands do reach the service is
 * `ServiceDownloadCommandsTest`'s concern.
 *
 * @param manager resolved per command, so a test can hand this to the
 *   repository that builds the manager.
 */
internal class InProcessDownloadCommands(
    private val manager: () -> DownloadManager,
) : DownloadCommands {
    override fun add(request: DownloadRequest) = manager().addDownload(request)

    override fun setStopReason(
        id: String,
        stopReason: Int,
    ) = manager().setStopReason(id, stopReason)

    override fun remove(id: String) = manager().removeDownload(id)

    override fun removeAll() = manager().removeAllDownloads()
}
