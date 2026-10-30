package dev.beam.android.feature.player

import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class PlayerTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `starting plays the requested file`() =
        runTest {
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()

            model.start(request())
            testScheduler.advanceUntilIdle()

            assertEquals("file-1", player.played.single().fileId)
        }

    @Test
    fun `skipping back and forward use the conventional intervals`() =
        runTest {
            // 10 back and 30 forward, matching what every other player on the
            // platform does; a viewer should not have to learn ours.
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()

            model.rewind()
            model.fastForward()

            assertEquals(listOf(-10_000L, 30_000L), player.seeks)
        }

    @Test
    fun `leaving the screen stops playback but does not release the shared player`() =
        runTest {
            // The ExoPlayer is a singleton the media session also holds. Releasing
            // it here would kill background playback the moment the screen went
            // away.
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()

            model.stop()

            assertTrue(player.stopped)
            assertFalse(player.released)
        }

    @Test
    fun `the settings sheet opens over the controls and a version change closes it`() =
        runTest {
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()
            model.setControlsVisible(false)

            model.setSettingsOpen(true)
            assertTrue(model.state.value.isSettingsOpen)
            assertTrue("the sheet is reached from the controls", model.state.value.areControlsVisible)

            model.switchSource(Fixtures.source(fileId = "file-remux"))
            testScheduler.advanceUntilIdle()

            assertFalse(model.state.value.isSettingsOpen)
            assertEquals(listOf("file-remux"), player.switched)
        }

    @Test
    fun `a picture-in-picture window closes the sheet, and closing the window pauses`() =
        runTest {
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            model.setSettingsOpen(true)

            model.setInPictureInPicture(true)
            assertTrue(model.state.value.isInPictureInPicture)
            assertFalse("no sheet reappears when the window grows back", model.state.value.isSettingsOpen)

            model.onPictureInPictureDismissed()
            testScheduler.advanceUntilIdle()

            assertFalse(player.state.value.isPlaying)
        }
}
