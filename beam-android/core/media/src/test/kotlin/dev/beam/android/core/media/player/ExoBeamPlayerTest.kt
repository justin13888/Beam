// Media3's test players are @UnstableApi, like the ExoPlayer surface itself.
@file:UnstableApi

package dev.beam.android.core.media.player

import android.net.Uri
import androidx.media3.common.PlaybackException
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
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
import uniffi.beam_client_core.SwitchTrigger

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
    fun `a stream refused for its session has the core check the session`() =
        runTest {
            val scope = TestScope(testScheduler)
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()
            // The server refuses the same cookie through the core too, which
            // is what moves the session to Expired and announces it.
            playback.failWith = BeamException.SessionExpired()

            player.onPlayerError(badStatus(401))
            scope.runCurrent()

            assertEquals(
                "one secured call puts the cookie to the server",
                listOf<UInt?>(1u),
                playback.continueWatchingLimits,
            )
            val error = player.state.value.error
            assertNotNull(error)
            assertTrue("the failure says to sign in", error!!.requiresSignIn)
            player.stop()
        }

    @Test
    fun `a stream refused with 401 while the core still honours the session is offered as a retry`() =
        runTest {
            val scope = TestScope(testScheduler)
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()

            player.onPlayerError(badStatus(401))
            scope.runCurrent()

            assertEquals(listOf<UInt?>(1u), playback.continueWatchingLimits)
            val error = player.state.value.error
            assertNotNull(error)
            assertFalse("nothing will send the viewer to sign in", error!!.requiresSignIn)
            assertTrue("the refusal came from in front of the server", error.isRetryable)
            assertEquals("The server refused to send this file.", error.message)
            player.stop()
        }

    @Test
    fun `a session check that fails for another reason does not leave the viewer at a sign-in prompt`() =
        runTest {
            val scope = TestScope(testScheduler)
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()
            playback.failWith = BeamException.Network("connection reset", retryable = true)

            player.onPlayerError(badStatus(401))
            scope.runCurrent()

            val error = player.state.value.error
            assertNotNull(error)
            assertFalse(error!!.requiresSignIn)
            assertTrue(error.isRetryable)
            player.stop()
        }

    @Test
    fun `a stream refused for access does not question the session`() =
        runTest {
            val scope = TestScope(testScheduler)
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()

            player.onPlayerError(badStatus(403))
            scope.runCurrent()

            assertTrue("an accepted session is not re-checked", playback.continueWatchingLimits.isEmpty())
            assertFalse(
                player.state.value.error!!
                    .requiresSignIn,
            )
            player.stop()
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

    @Test
    fun `switching from a later part to a whole file lands at the same moment of the film`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[FILE_ID] = twoPartPlan(startIndex = 1u)
            val player = player(scope)
            player.play(REQUEST.copy(startPositionSecs = 600.0))
            scope.runCurrent()

            player.switchSource(WHOLE_FILE_ID, SwitchTrigger.MANUAL)
            scope.runCurrent()

            assertEquals(WHOLE_FILE_ID, exoPlayer.getMediaItemAt(0).mediaId)
            assertEquals(0, exoPlayer.currentMediaItemIndex)
            assertEquals("part 2, 10 minutes in, is 1h 0m into the film", 3_600_000L, exoPlayer.currentPosition)
            player.stop()
        }

    @Test
    fun `switching from a whole file to a multi-part version lands in the part that holds the moment`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[MULTI_PART_ID] = twoPartPlan(startIndex = 0u)
            val player = player(scope)
            player.play(REQUEST.copy(fileId = WHOLE_FILE_ID, startPositionSecs = 3_600.0))
            scope.runCurrent()

            player.switchSource(MULTI_PART_ID, SwitchTrigger.MANUAL)
            scope.runCurrent()

            assertEquals(1, exoPlayer.currentMediaItemIndex)
            assertEquals(600_000L, exoPlayer.currentPosition)
            assertEquals(1, player.state.value.partIndex)
            player.stop()
        }

    /** Fifty minutes, then forty-six and forty seconds. */
    private fun twoPartPlan(startIndex: UInt) =
        Fixtures.plan(
            parts =
                listOf(
                    Fixtures.part(FILE_ID, durationSecs = 3_000.0),
                    Fixtures.part("part-2", durationSecs = 2_800.0),
                ),
            startIndex = startIndex,
        )

    /** What Media3 reports when the stream's server answers [status]. */
    private fun badStatus(status: Int) =
        PlaybackException(
            "bad status",
            HttpDataSource.InvalidResponseCodeException(
                status,
                null,
                null,
                emptyMap(),
                DataSpec(Uri.parse("https://beam.test/v1/files/$FILE_ID/stream")),
                ByteArray(0),
            ),
            PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
        )

    /** A monitor that never judges rebuffering sustained; the rule is the core's, tested there. */
    private object NeverSustained : RebufferMonitorInterface {
        override fun stallStarted(nowMs: ULong): Boolean = false

        override fun stallEnded(nowMs: ULong): Boolean = false

        override fun poll(nowMs: ULong): Boolean = false

        override fun reset() = Unit
    }

    private companion object {
        const val FILE_ID = "file-1"
        const val WHOLE_FILE_ID = "whole-file"
        const val MULTI_PART_ID = "multi-part"
        val REQUEST =
            PlaybackRequest(
                mediaId = "movie-1",
                fileId = FILE_ID,
                title = "Le Samourai",
                durationSecs = 3_000.0,
            )
    }
}
