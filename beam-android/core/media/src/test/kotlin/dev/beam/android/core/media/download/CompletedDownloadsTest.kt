// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.database.SQLException
import android.net.Uri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.DatabaseIOException
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.cache.CacheWriter
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadCursor
import androidx.media3.exoplayer.offline.DownloadIndex
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode
import uniffi.beam_client_core.DownloadState

/**
 * Which downloads count as complete, and that a finished or failed one stays
 * listed, through the harness's real manager, core and server: [DONE] is
 * served in full under a fresh session, and every request for [PARTIAL],
 * whose first bytes are already in the cache, is refused.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class CompletedDownloadsTest : DownloadRepositoryHarness() {
    @Test
    fun `only a download that has completed counts as downloaded`() =
        runTest {
            sessions[serverId(HOME)] = config(FRESH, HOME)
            sessions[serverId(CABIN)] = config(STALE, CABIN)
            // The first bytes of PARTIAL, as an interrupted download leaves them.
            CacheWriter(
                CacheDataSource(cache, ByteArrayDataSource(BODY.toByteArray())),
                DataSpec
                    .Builder()
                    .setUri(Uri.parse(mediaUrl(PARTIAL, CABIN)))
                    .setLength(4)
                    .build(),
                null,
                null,
            ).cache()

            // DONE first: the manager runs one download at a time, and PARTIAL
            // never finishes.
            enqueue(DONE, on = HOME)
            awaitUntil { indexedState(DONE) == Download.STATE_COMPLETED }
            enqueue(PARTIAL, on = CABIN)
            awaitUntil { indexedState(PARTIAL) != null }

            assertTrue(cache.getCachedSpans(mediaUrl(PARTIAL, CABIN)).isNotEmpty())
            assertFalse("bytes on disk are not a finished download", repository.isDownloaded(PARTIAL))
            assertNull(repository.completed(PARTIAL))
            assertTrue(repository.isDownloaded(DONE))
            assertEquals(CompletedDownload(DONE, mediaUrl(DONE, HOME)), repository.completed(DONE))
            assertNull("never queued", repository.completed("unknown"))
        }

    @Test
    fun `a download stays listed once it has completed or failed`() =
        runTest {
            // Media3 waits seconds between attempts; failing on the first
            // keeps this to the outcome rather than the backoff.
            repository.manager().minRetryCount = 0
            sessions[serverId(HOME)] = config(FRESH, HOME)
            sessions[serverId(CABIN)] = config(STALE, CABIN)
            observe()

            enqueue(DONE, on = HOME)
            enqueue(PARTIAL, on = CABIN)
            awaitUntil { stateOf(DONE) == DownloadState.COMPLETED && stateOf(PARTIAL) == DownloadState.FAILED }

            assertEquals(
                setOf(DONE to DownloadState.COMPLETED, PARTIAL to DownloadState.FAILED),
                listed.map { it.fileId to it.state }.toSet(),
            )
        }

    @Test
    fun `resuming a failed download retries it`() =
        runTest {
            repository.manager().minRetryCount = 0
            val cabin = serverId(CABIN)
            sessions[cabin] = config(STALE, CABIN)
            enqueue(RETRIED, on = CABIN)
            awaitUntil { indexedState(RETRIED) == Download.STATE_FAILED }

            // The viewer signs in again, then retries.
            sessions[cabin] = config(FRESH, CABIN)
            repository.resume(RETRIED)
            awaitUntil { indexedState(RETRIED) == Download.STATE_COMPLETED }

            assertEquals(CompletedDownload(RETRIED, mediaUrl(RETRIED, CABIN)), repository.completed(RETRIED))
        }

    @Test
    fun `an index that cannot be read counts as no completed download`() {
        // What Media3's SQLite index throws when its database cannot be read.
        val unreadable =
            object : DownloadIndex {
                override fun getDownload(id: String): Download? =
                    throw DatabaseIOException(SQLException("disk I/O error"))

                override fun getDownloads(vararg states: Int): DownloadCursor =
                    throw DatabaseIOException(SQLException("disk I/O error"))
            }

        assertNull(unreadable.completedDownload(DONE))
    }

    private fun indexedState(fileId: String): Int? = indexed().firstOrNull { it.request.id == fileId }?.state

    private companion object {
        const val DONE = "d1000000-0000-4000-8000-000000000001"
        const val PARTIAL = "e1000000-0000-4000-8000-000000000001"
        const val RETRIED = "f2000000-0000-4000-8000-000000000001"
    }
}
