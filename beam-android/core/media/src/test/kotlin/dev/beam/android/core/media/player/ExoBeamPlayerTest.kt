// Media3's test players are @UnstableApi, like the ExoPlayer surface itself.
@file:UnstableApi

package dev.beam.android.core.media.player

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.test.utils.TestExoPlayerBuilder
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakeTelemetryRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.RebufferMonitorInterface

/**
 * The player over a real ExoPlayer -- Media3's test build of it, on
 * Robolectric's main looper -- with the core's repositories faked below the
 * interface line. Nothing here waits on the network: what is asserted is what
 * the player does on the call, before any byte of media could arrive.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class ExoBeamPlayerTest {
    private val exoPlayer: ExoPlayer = TestExoPlayerBuilder(RuntimeEnvironment.getApplication()).build()
    private val playback = FakePlaybackRepository()
    private val telemetry = FakeTelemetryRepository()

    @After
    fun tearDown() {
        exoPlayer.release()
    }

    private fun player(scope: TestScope) =
        ExoBeamPlayer(
            context = RuntimeEnvironment.getApplication(),
            exoPlayer = exoPlayer,
            repository = playback,
            telemetry = telemetry,
            clients = BeamHttpClientFactory(BeamHttpClientFactory.shared()),
            scope = scope,
            rebuffers = NeverSustained,
        )

    @Test
    fun `pausing forces a progress report`() =
        runTest {
            val scope = TestScope(testScheduler)
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()
            assertTrue("nothing is reported before the first interval", playback.reportedProgress.isEmpty())

            player.pause()
            scope.runCurrent()

            // A paused viewer may close the app next; waiting for the
            // interval would lose where they stopped.
            val report = playback.reportedProgress.single()
            assertEquals(FILE_ID, report.fileId)
            assertTrue("the core is told to bypass its throttle", report.force)
            assertFalse(exoPlayer.playWhenReady)
            player.stop()
        }

    @Test
    fun `a plan that cannot be built is a playback error`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.failWith = BeamException.NotFound("This title has no playable files", "about:blank")
            val player = player(scope)

            player.play(REQUEST)
            scope.runCurrent()

            val state = player.state.value
            assertNotNull("the failure reaches the screen", state.error)
            assertEquals(REQUEST, state.request)
            assertEquals(Pair<String?, String>(REQUEST.mediaId, FILE_ID), playback.planRequests.single())
            assertEquals("nothing was loaded into the player", 0, exoPlayer.mediaItemCount)
            assertFalse("and nothing is set to play", exoPlayer.playWhenReady)

            // With no file current, a pause has no position to force.
            player.pause()
            scope.runCurrent()
            assertTrue("no position is reported for a file that never started", playback.reportedProgress.isEmpty())
        }

    @Test
    fun `a plan loads one item per part and starts in the part it names`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[FILE_ID] =
                Fixtures.plan(
                    parts = listOf(Fixtures.part(FILE_ID), Fixtures.part("part-2"), Fixtures.part("part-3")),
                    startIndex = 1u,
                )
            val player = player(scope)

            player.play(REQUEST.copy(startPositionSecs = 12.0))
            scope.runCurrent()

            assertEquals(
                listOf(FILE_ID, "part-2", "part-3"),
                (0 until exoPlayer.mediaItemCount).map {
                    exoPlayer.getMediaItemAt(it).mediaId
                },
            )
            assertEquals(1, exoPlayer.currentMediaItemIndex)
            assertEquals(12_000L, exoPlayer.currentPosition)
            assertTrue(exoPlayer.playWhenReady)
            val state = player.state.value
            assertEquals(1, state.partIndex)
            assertEquals(3, state.partCount)
            player.stop()
        }

    /** A monitor that never judges rebuffering sustained; the rule is the core's, tested there. */
    private object NeverSustained : RebufferMonitorInterface {
        override fun stallStarted(nowMs: ULong): Boolean = false

        override fun stallEnded(nowMs: ULong): Boolean = false

        override fun poll(nowMs: ULong): Boolean = false

        override fun reset() = Unit
    }

    private companion object {
        const val FILE_ID = "file-1"
        val REQUEST =
            PlaybackRequest(
                mediaId = "movie-1",
                fileId = FILE_ID,
                title = "Le Samourai",
                durationSecs = 3_000.0,
            )
    }
}
