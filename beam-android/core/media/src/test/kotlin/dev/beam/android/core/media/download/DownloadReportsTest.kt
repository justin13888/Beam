// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.database.SQLException
import androidx.core.net.toUri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.DatabaseIOException
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadCursor
import androidx.media3.exoplayer.offline.DownloadIndex
import androidx.media3.exoplayer.offline.DownloadProgress
import androidx.media3.exoplayer.offline.DownloadRequest
import androidx.media3.exoplayer.scheduler.Requirements
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.beam_client_core.DownloadState

/** What Media3 says about an item, in the core's terms. */
@RunWith(RobolectricTestRunner::class)
class DownloadReportsTest {
    @Test
    fun `each Media3 state reads as the core's`() {
        val table =
            listOf(
                Triple(Download.STATE_QUEUED, 0, DownloadState.QUEUED),
                // Queued behind a network the cellular preference forbids.
                Triple(Download.STATE_QUEUED, Requirements.NETWORK_UNMETERED, DownloadState.WAITING_FOR_NETWORK),
                Triple(Download.STATE_DOWNLOADING, 0, DownloadState.DOWNLOADING),
                Triple(Download.STATE_RESTARTING, 0, DownloadState.DOWNLOADING),
                Triple(Download.STATE_COMPLETED, 0, DownloadState.COMPLETED),
                Triple(Download.STATE_FAILED, 0, DownloadState.FAILED),
                // A stop is only ever the viewer's pause here.
                Triple(Download.STATE_STOPPED, 0, DownloadState.PAUSED),
            )
        for ((media3, unmet, expected) in table) {
            assertEquals("Media3 state $media3, unmet $unmet", expected, download(media3).toReport(unmet).state)
        }
    }

    @Test
    fun `a length Media3 does not know yet is reported as unknown, not as negative`() {
        val report = download(Download.STATE_DOWNLOADING, downloaded = 10L, length = -1L).toReport(0)

        assertNull(report.totalBytes)
        assertEquals(10uL, report.downloadedBytes)
        assertEquals(ITEM, report.itemId)
        assertEquals(400uL, download(Download.STATE_DOWNLOADING, length = 400L).toReport(0).totalBytes)
    }

    @Test
    fun `only a failure carries a message`() {
        assertTrue(download(Download.STATE_FAILED).toReport(0).failureMessage!!.contains("retry"))
        assertNull(download(Download.STATE_DOWNLOADING).toReport(0).failureMessage)
    }

    @Test
    fun `an index that cannot be read lists no downloads rather than failing the list`() {
        assertEquals(emptyList<Download>(), UNREADABLE.indexedDownloads())
    }

    @Test
    fun `an index that cannot be read has no failed request, so a resume clears the stop reason`() {
        assertNull(UNREADABLE.failedRequest(ITEM))
    }

    private fun download(
        state: Int,
        downloaded: Long = 0L,
        length: Long = -1L,
    ) = Download(
        DownloadRequest.Builder(ITEM, "https://beam.test/v1/files/$ITEM/download".toUri()).build(),
        state,
        0L,
        0L,
        length,
        if (state == Download.STATE_STOPPED) BeamDownloadManager.STOP_REASON_USER else Download.STOP_REASON_NONE,
        if (state == Download.STATE_FAILED) Download.FAILURE_REASON_UNKNOWN else Download.FAILURE_REASON_NONE,
        DownloadProgress().apply { bytesDownloaded = downloaded },
    )

    private companion object {
        const val ITEM = "f1000000-0000-4000-8000-000000000001"

        /** What Media3's SQLite index throws when its database cannot be read. */
        val UNREADABLE =
            object : DownloadIndex {
                override fun getDownload(id: String): Download? =
                    throw DatabaseIOException(SQLException("disk I/O error"))

                override fun getDownloads(vararg states: Int): DownloadCursor =
                    throw DatabaseIOException(SQLException("disk I/O error"))
            }
    }
}
