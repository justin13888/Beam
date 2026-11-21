// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.database.SQLException
import android.net.Uri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.DatabaseIOException
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.exoplayer.offline.DefaultDownloadIndex
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadCursor
import androidx.media3.exoplayer.offline.DownloadIndex
import androidx.media3.exoplayer.offline.DownloadProgress
import androidx.media3.exoplayer.offline.DownloadRequest
import androidx.test.core.app.ApplicationProvider
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/**
 * How the downloads list is put together from Media3's SQLite download index
 * -- the real one, on Robolectric's SQLite -- and the manager's live
 * `currentDownloads`.
 */
@RunWith(RobolectricTestRunner::class)
class ListedDownloadsTest {
    private lateinit var database: StandaloneDatabaseProvider
    private lateinit var index: DefaultDownloadIndex

    @Before
    fun setUp() {
        database = StandaloneDatabaseProvider(ApplicationProvider.getApplicationContext())
        index = DefaultDownloadIndex(database)
    }

    @After
    fun tearDown() {
        database.close()
    }

    @Test
    fun `finished downloads stay listed, in progress ones are the live copy, unindexed ones come last`() {
        index.putDownload(download("done", Download.STATE_COMPLETED, startedAt = 1L, bytes = 900L))
        index.putDownload(download("broken", Download.STATE_FAILED, startedAt = 2L, bytes = 10L))
        index.putDownload(download("going", Download.STATE_DOWNLOADING, startedAt = 3L, bytes = 100L))
        val live = download("going", Download.STATE_DOWNLOADING, startedAt = 3L, bytes = 500L)
        val unindexed = download("new", Download.STATE_QUEUED, startedAt = 4L, bytes = 0L)

        val listed = index.listed(current = listOf(unindexed, live))

        assertEquals(
            listOf(
                "done" to Download.STATE_COMPLETED,
                "broken" to Download.STATE_FAILED,
                "going" to Download.STATE_DOWNLOADING,
                "new" to Download.STATE_QUEUED,
            ),
            listed.map { it.request.id to it.state },
        )
        assertEquals("the live progress, not the index's last write", 500L, listed[2].bytesDownloaded)
    }

    @Test
    fun `an index that cannot be read lists the downloads in progress`() {
        // What Media3's SQLite index throws when its database cannot be read.
        val unreadable =
            object : DownloadIndex {
                override fun getDownload(id: String): Download? =
                    throw DatabaseIOException(SQLException("disk I/O error"))

                override fun getDownloads(vararg states: Int): DownloadCursor =
                    throw DatabaseIOException(SQLException("disk I/O error"))
            }
        val current = listOf(download("going", Download.STATE_DOWNLOADING, startedAt = 1L, bytes = 100L))

        assertEquals(current, unreadable.listed(current))
    }

    private fun download(
        id: String,
        state: Int,
        startedAt: Long,
        bytes: Long,
    ): Download =
        Download(
            DownloadRequest.Builder(id, Uri.parse("https://beam.test/v1/files/$id/stream")).build(),
            state,
            startedAt,
            startedAt,
            CONTENT_LENGTH,
            Download.STOP_REASON_NONE,
            if (state == Download.STATE_FAILED) Download.FAILURE_REASON_UNKNOWN else Download.FAILURE_REASON_NONE,
            DownloadProgress().apply {
                bytesDownloaded = bytes
                percentDownloaded = bytes * 100f / CONTENT_LENGTH
            },
        )

    private companion object {
        const val CONTENT_LENGTH = 1_000L
    }
}
