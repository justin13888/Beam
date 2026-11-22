// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.Download
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState

/**
 * The downloads repository's single-file downloads, through the harness's
 * real manager, core and server: sessions, servers, and what the core is
 * told about each.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class MediaDownloadRepositoryTest : DownloadRepositoryHarness() {
    @Test
    fun `downloads are listed while signed out`() =
        runTest {
            observe()

            enqueue(F1, on = HOME)
            awaitUntil { listed.isNotEmpty() }

            assertEquals(listOf(F1), listed.map(DownloadRecord::fileId))
            assertEquals("its server is the one it was queued on", serverId(HOME), listed.single().serverId)
        }

    @Test
    fun `a download picks up a new session on its next request without the manager being rebuilt`() =
        runTest {
            val home = serverId(HOME)
            sessions[home] = config(STALE, HOME)
            // The viewer signs in again while the first attempt is in flight.
            onRefused = { sessions[home] = config(FRESH, HOME) }
            observe()

            enqueue(F1, on = HOME)
            awaitUntil { stateOf(F1) == DownloadState.COMPLETED }

            assertEquals(listOf<String?>(STALE, FRESH), synchronized(cookies) { cookies.toList() })
            assertTrue("the completed download plays offline", repository.isDownloaded(F1))
        }

    @Test
    fun `a download from a server that is not the active one completes under its own server's session`() =
        runTest {
            sessions[serverId(HOME)] = config(FRESH, HOME)
            sessions[serverId(CABIN)] = config("beam_session=cabin", CABIN)
            observe()

            // Queued from home, then the viewer switched to signed-in cabin.
            enqueue(F1, on = HOME)
            core.selectServer(serverId(CABIN))
            awaitUntil { stateOf(F1) == DownloadState.COMPLETED }

            assertEquals(listOf<String?>(FRESH), synchronized(cookies) { cookies.toList() })
        }

    @Test
    fun `forgetting a server deletes its downloads and leaves other servers' alone`() =
        runTest {
            observe()
            sessions[serverId(HOME)] = config(FRESH, HOME)
            sessions[serverId(CABIN)] = config(FRESH, CABIN)
            enqueue(H1, on = HOME)
            enqueue(C1, on = CABIN)
            awaitUntil { stateOf(H1) == DownloadState.COMPLETED && stateOf(C1) == DownloadState.COMPLETED }

            repository.removeServer(serverId(CABIN))
            awaitUntil { indexed().none { it.request.id == C1 } }

            assertEquals(listOf(H1), core.downloads().map(DownloadRecord::fileId))
            assertTrue("the removed download's bytes are gone", cache.getCachedSpans(mediaUrl(C1, CABIN)).isEmpty())
            assertTrue("the kept download's bytes stay", cache.getCachedSpans(mediaUrl(H1, HOME)).isNotEmpty())
        }

    @Test
    fun `a download that finishes while nothing collects is listed complete once something does`() =
        runTest {
            sessions[serverId(HOME)] = config(FRESH, HOME)

            // The downloads screen is closed: nothing collects, so nothing
            // reports to the core while the transfer runs and finishes.
            enqueue(F1, on = HOME)
            awaitUntil { indexed().any { it.request.id == F1 && it.state == Download.STATE_COMPLETED } }
            assertEquals(DownloadState.QUEUED, core.downloads().single().state)
            // The detail screen reads this with the downloads screen still
            // closed, so it must not wait on the core's record to catch up.
            assertTrue(
                "the completed download plays offline before anything collects",
                repository.isDownloaded(F1),
            )

            observe()
            awaitUntil { stateOf(F1) == DownloadState.COMPLETED }

            assertTrue("the completed download plays offline", repository.isDownloaded(F1))
        }

    private companion object {
        const val F1 = "f1000000-0000-4000-8000-000000000001"
        const val H1 = "a1000000-0000-4000-8000-000000000001"
        const val C1 = "c1000000-0000-4000-8000-000000000001"
    }
}
