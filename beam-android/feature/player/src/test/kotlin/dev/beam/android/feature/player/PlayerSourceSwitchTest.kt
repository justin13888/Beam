package dev.beam.android.feature.player

import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.SwitchTrigger

/** Changing the file that plays: on request, on sustained rebuffering, and undoing it. */
@OptIn(ExperimentalCoroutinesApi::class)
class PlayerSourceSwitchTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `switching source asks the player to hold position`() =
        runTest {
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()

            model.switchSource(Fixtures.source(fileId = "file-hd"))
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-hd"), player.switched)
            assertEquals(
                "the viewer chose it, so it is counted as a manual switch",
                listOf(SwitchTrigger.MANUAL),
                player.switchTriggers,
            )
        }

    @Test
    fun `sustained rebuffering switches to the smaller file the core chose and says so`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = smallerSource() }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-sd"), player.switched)
            assertEquals(
                "the screen chose it for the viewer, so it is counted as automatic",
                listOf(SwitchTrigger.AUTO),
                player.switchTriggers,
            )
            assertEquals(
                SourceDowngrade(message = DOWNGRADE_REASON, fromFileId = "file-1"),
                model.state.value.downgrade,
            )
            assertEquals(
                "an episode's sources are asked for by the episode, never by its show",
                listOf("episode-1" to "file-1"),
                playback.downgradeRequests,
            )
        }

    @Test
    fun `a movie's fallback is asked for by the movie`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = smallerSource() }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request().copy(episodeId = null))
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("media-1" to "file-1"), playback.downgradeRequests)
        }

    @Test
    fun `a source the viewer picked is never overridden`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = smallerSource() }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            model.switchSource(Fixtures.source(fileId = "file-remux"))
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-remux"), player.switched)
            assertTrue(playback.downgradeRequests.isEmpty())
            assertNull(model.state.value.downgrade)
        }

    @Test
    fun `a source picked before playback is never overridden`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = smallerSource() }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request().copy(isSourcePicked = true))
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value.isSourcePinned)
            assertTrue(player.switched.isEmpty())
            assertTrue(playback.downgradeRequests.isEmpty())
            assertNull(model.state.value.downgrade)
        }

    @Test
    fun `starting again does not drop a pin set in the player`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            model.switchSource(Fixtures.source(fileId = "file-remux"))
            testScheduler.advanceUntilIdle()

            // The route starts the request again when it is rebuilt, as after a rotation.
            model.start(request())
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value.isSourcePinned)
        }

    @Test
    fun `a pick made while the core is asked wins over its answer`() =
        runTest {
            val player = FakeBeamPlayer()
            val hold = CompletableDeferred<Unit>()
            val playback =
                FakePlaybackRepository().apply {
                    downgrade = smallerSource()
                    downgradeHold = hold
                }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            // The viewer pins the very file that is playing, so only the pin,
            // not a change of file, can be what drops the answer.
            model.switchSource(Fixtures.source(fileId = "file-1"))
            testScheduler.advanceUntilIdle()
            hold.complete(Unit)
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("episode-1" to "file-1"), playback.downgradeRequests)
            assertEquals(listOf("file-1"), player.switched)
            assertNull(model.state.value.downgrade)
        }

    @Test
    fun `an answer about a file that is no longer playing is dropped`() =
        runTest {
            val player = FakeBeamPlayer()
            val hold = CompletableDeferred<Unit>()
            val playback =
                FakePlaybackRepository().apply {
                    downgrade = smallerSource()
                    downgradeHold = hold
                }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            // Something else (the media session's queue, say) moved playback
            // on without the viewer pinning anything.
            player.play(request().copy(fileId = "file-9"))
            testScheduler.advanceUntilIdle()
            hold.complete(Unit)
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("episode-1" to "file-1"), playback.downgradeRequests)
            assertFalse(model.state.value.isSourcePinned)
            assertTrue(player.switched.isEmpty())
            assertNull(model.state.value.downgrade)
        }

    @Test
    fun `with nothing smaller to play the current file keeps playing`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = null }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertTrue(player.switched.isEmpty())
            assertNull(model.state.value.downgrade)
        }

    @Test
    fun `a failure asking the core keeps the current file playing`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback =
                FakePlaybackRepository().apply {
                    downgrade = smallerSource()
                    failWith = BeamException.NotFound("gone", "about:blank")
                }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertTrue(player.switched.isEmpty())
            assertNull(model.state.value.downgrade)
        }

    @Test
    fun `one verdict on a file asks the core once`() =
        runTest {
            // The verdict stays set while the file plays, and every position
            // tick republishes the state; asking on each would hammer the
            // server with a sources request twice a second.
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = null }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()
            player.seekTo(60_000L)
            testScheduler.advanceUntilIdle()

            assertEquals(1, playback.downgradeRequests.size)
        }

    @Test
    fun `undoing a downgrade returns to the original file and pins it`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = smallerSource() }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            model.undoDowngrade()
            testScheduler.advanceUntilIdle()
            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-sd", "file-1"), player.switched)
            assertEquals(listOf(SwitchTrigger.AUTO, SwitchTrigger.MANUAL), player.switchTriggers)
            assertTrue(model.state.value.isSourcePinned)
            assertNull(model.state.value.downgrade)
            assertEquals(
                "going back is a choice, so the original file is not downgraded again",
                1,
                playback.downgradeRequests.size,
            )
        }

    @Test
    fun `dismissing the notice keeps the smaller file`() =
        runTest {
            val player = FakeBeamPlayer()
            val playback = FakePlaybackRepository().apply { downgrade = smallerSource() }
            val model = viewModel(player = player, playback = playback)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            player.rebufferPersistently()
            testScheduler.advanceUntilIdle()

            model.dismissDowngrade()
            testScheduler.advanceUntilIdle()

            assertNull(model.state.value.downgrade)
            assertEquals(listOf("file-sd"), player.switched)
            assertFalse(model.state.value.isSourcePinned)
        }
}
