package dev.beam.android.feature.player

import androidx.lifecycle.SavedStateHandle
import dev.beam.android.core.media.session.PlayerProvider
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
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

    private fun viewModel(
        player: FakeBeamPlayer = FakeBeamPlayer(),
        catalog: FakeCatalogRepository =
            FakeCatalogRepository().apply {
                nextEpisode = Fixtures.episode(id = "episode-2", fileId = "file-2")
            },
        preferences: FakePreferencesRepository = FakePreferencesRepository(),
    ) = PlayerViewModel(
        player = player,
        // Throws rather than returning a stub: the view model must not build a
        // decoder simply to exist, and only the video surface should ever ask
        // for one. If this fires, that has regressed.
        playerProvider =
            object : PlayerProvider {
                override fun exoPlayer(): androidx.media3.exoplayer.ExoPlayer =
                    error("the player screen must not construct a decoder to hold state")
            },
        playback = FakePlaybackRepository(),
        catalog = catalog,
        preferences = preferences,
        savedStateHandle =
            SavedStateHandle(
                mapOf(
                    "fileId" to "file-1",
                    "mediaId" to "media-1",
                    "episodeId" to "episode-1",
                ),
            ),
    )

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
    fun `the up-next episode is resolved before it is needed`() =
        runTest {
            // Resolved up front so the prompt can appear the instant the title
            // ends; fetching it at that moment would show an empty prompt or none.
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            assertEquals(
                "episode-2",
                model.state.value.upNext
                    ?.id,
            )
        }

    @Test
    fun `finishing auto-advances when the viewer asked for that`() =
        runTest {
            val player = FakeBeamPlayer()
            val model =
                viewModel(
                    player = player,
                    preferences = FakePreferencesRepository(UserPreferences(autoPlayNext = true)),
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.finish()
            testScheduler.advanceUntilIdle()

            assertEquals("file-2", player.played.last().fileId)
            assertFalse(model.state.value.isOfferingNext)
        }

    @Test
    fun `finishing offers the next episode when auto-advance is off`() =
        runTest {
            val player = FakeBeamPlayer()
            val model =
                viewModel(
                    player = player,
                    preferences = FakePreferencesRepository(UserPreferences(autoPlayNext = false)),
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.finish()
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value.isOfferingNext)
            assertEquals(1, player.played.size)
        }

    @Test
    fun `an episode with no file on this server is never offered`() =
        runTest {
            // Auto-playing into an episode the server does not have would stop
            // playback with an error the viewer did not ask for.
            val player = FakeBeamPlayer()
            val model =
                viewModel(
                    player = player,
                    catalog =
                        FakeCatalogRepository().apply {
                            nextEpisode = Fixtures.episode(id = "episode-2", fileId = null)
                        },
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.finish()
            testScheduler.advanceUntilIdle()

            assertFalse(model.state.value.isOfferingNext)
            assertEquals(1, player.played.size)
        }

    @Test
    fun `finishing the last episode of a series simply stops`() =
        runTest {
            val player = FakeBeamPlayer()
            val model =
                viewModel(
                    player = player,
                    catalog = FakeCatalogRepository().apply { nextEpisode = null },
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.finish()
            testScheduler.advanceUntilIdle()

            assertFalse(model.state.value.isOfferingNext)
        }

    @Test
    fun `dismissing the prompt does not play the next episode`() =
        runTest {
            val player = FakeBeamPlayer()
            val model =
                viewModel(
                    player = player,
                    preferences = FakePreferencesRepository(UserPreferences(autoPlayNext = false)),
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            player.finish()
            testScheduler.advanceUntilIdle()

            model.dismissUpNext()

            assertFalse(model.state.value.isOfferingNext)
            assertEquals(1, player.played.size)
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
    fun `switching source asks the player to hold position`() =
        runTest {
            val player = FakeBeamPlayer()
            val model = viewModel(player = player)
            testScheduler.advanceUntilIdle()

            model.switchSource(Fixtures.source(fileId = "file-hd"))
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-hd"), player.switched)
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

    private fun request() =
        PlaybackRequest(
            mediaId = "media-1",
            episodeId = "episode-1",
            fileId = "file-1",
            title = "Arrival",
        )
}
