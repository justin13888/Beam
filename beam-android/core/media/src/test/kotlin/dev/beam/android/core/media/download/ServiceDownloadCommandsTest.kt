// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.app.Application
import android.content.Intent
import androidx.core.net.toUri
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadRequest
import androidx.media3.exoplayer.offline.DownloadService
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf

/**
 * Every change to a download starts [BeamDownloadService] with it, so the
 * transfer runs in the foreground service rather than in whatever process
 * happened to ask.
 */
@RunWith(RobolectricTestRunner::class)
class ServiceDownloadCommandsTest {
    private val application: Application = ApplicationProvider.getApplicationContext()
    private val commands = ServiceDownloadCommands(application)

    @Test
    fun `an enqueue starts the download service with the request`() {
        val request = DownloadRequest.Builder("f1", "https://beam.test/v1/files/f1/stream".toUri()).build()

        commands.add(request)

        val sent = nextStartedService(DownloadService.ACTION_ADD_DOWNLOAD)

        @Suppress("DEPRECATION")
        val carried = sent.getParcelableExtra<DownloadRequest>(DownloadService.KEY_DOWNLOAD_REQUEST)
        assertEquals(request, carried)
    }

    @Test
    fun `a pause and a resume set the download's stop reason through the service`() {
        commands.setStopReason("f1", BeamDownloadManager.STOP_REASON_USER)
        val paused = nextStartedService(DownloadService.ACTION_SET_STOP_REASON)
        commands.setStopReason("f1", Download.STOP_REASON_NONE)
        val resumed = nextStartedService(DownloadService.ACTION_SET_STOP_REASON)

        assertEquals("f1", paused.getStringExtra(DownloadService.KEY_CONTENT_ID))
        assertEquals(BeamDownloadManager.STOP_REASON_USER, paused.getIntExtra(DownloadService.KEY_STOP_REASON, -1))
        assertEquals("f1", resumed.getStringExtra(DownloadService.KEY_CONTENT_ID))
        assertEquals(Download.STOP_REASON_NONE, resumed.getIntExtra(DownloadService.KEY_STOP_REASON, -1))
    }

    @Test
    fun `a removal goes through the service`() {
        commands.remove("f1")
        val one = nextStartedService(DownloadService.ACTION_REMOVE_DOWNLOAD)
        commands.removeAll()
        nextStartedService(DownloadService.ACTION_REMOVE_ALL_DOWNLOADS)

        assertEquals("f1", one.getStringExtra(DownloadService.KEY_CONTENT_ID))
    }

    /** The next service start, asserted to be [action] sent to [BeamDownloadService]. */
    private fun nextStartedService(action: String): Intent {
        val sent = shadowOf(application).nextStartedService
        assertNotNull("a service was started", sent)
        assertEquals(BeamDownloadService::class.java.name, sent.component?.className)
        assertEquals(action, sent.action)
        return sent
    }
}
