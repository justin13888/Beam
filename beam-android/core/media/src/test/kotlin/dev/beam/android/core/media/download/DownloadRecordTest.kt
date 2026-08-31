package dev.beam.android.core.media.download

import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class DownloadRecordTest {
    @Test
    fun `progress is null before the total size is known`() {
        // Media3 reports no content length until the server has answered, and
        // a determinate bar starting from a made-up total is worse than an
        // indeterminate one.
        val record = record(downloaded = 1_000L, total = 0L)

        assertNull(record.progress)
    }

    @Test
    fun `progress is the fraction fetched`() {
        assertEquals(0.5f, record(downloaded = 500L, total = 1_000L).progress!!, 0.0001f)
    }

    @Test
    fun `only a completed download is playable offline`() {
        assertTrue(record(state = DownloadState.Completed).isPlayableOffline)
        assertFalse(record(state = DownloadState.Downloading).isPlayableOffline)
        assertFalse(record(state = DownloadState.Paused).isPlayableOffline)
        assertFalse(record(state = DownloadState.Failed).isPlayableOffline)
    }

    private fun record(
        downloaded: Long = 0L,
        total: Long = 0L,
        state: DownloadState = DownloadState.Downloading,
    ) = DownloadRecord(
        fileId = "f1",
        mediaId = "m1",
        serverId = "s1",
        title = "Arrival",
        state = state,
        downloadedBytes = downloaded,
        totalBytes = total,
    )
}
