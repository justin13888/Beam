package dev.beam.android.feature.player

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

/** What follows an episode: resolving it, offering it, and advancing to it. */
@OptIn(ExperimentalCoroutinesApi::class)
class PlayerUpNextTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

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
    fun `the next episode is not held to the last episode's pick`() =
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
            model.switchSource(Fixtures.source(fileId = "file-remux"))
            testScheduler.advanceUntilIdle()

            player.finish()
            testScheduler.advanceUntilIdle()

            assertEquals("file-2", player.played.last().fileId)
            assertFalse(model.state.value.isSourcePinned)
        }

    @Test
    fun `advancing through a series asks what follows the episode now playing`() =
        runTest {
            // Up-next is the server's answer per episode. Holding on to the
            // answer for the episode the screen opened on replayed the second
            // episode at the end of the second episode.
            val player = FakeBeamPlayer()
            val catalog =
                FakeCatalogRepository().apply {
                    nextEpisodes["episode-1"] = Fixtures.episode(id = "episode-2", fileId = "file-2")
                    nextEpisodes["episode-2"] = Fixtures.episode(id = "episode-3", fileId = "file-3")
                }
            val model =
                viewModel(
                    player = player,
                    catalog = catalog,
                    preferences = FakePreferencesRepository(UserPreferences(autoPlayNext = true)),
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()

            player.finish()
            testScheduler.advanceUntilIdle()
            player.finish()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-1", "file-2", "file-3"), player.played.map { it.fileId })
            assertEquals(listOf("episode-1", "episode-2", "episode-3"), catalog.upNextCalls.map { it.second })
        }

    @Test
    fun `an episode's versions are asked for by the episode, not its show`() =
        runTest {
            // A show has no sources of its own; asking by the show listed none
            // and left the version switcher empty for every episode.
            val playback = FakePlaybackRepository()
            val catalog =
                FakeCatalogRepository().apply {
                    nextEpisodes["episode-1"] = Fixtures.episode(id = "episode-2", fileId = "file-2")
                }
            val player = FakeBeamPlayer()
            val model =
                viewModel(
                    player = player,
                    catalog = catalog,
                    playback = playback,
                    preferences = FakePreferencesRepository(UserPreferences(autoPlayNext = true)),
                )
            testScheduler.advanceUntilIdle()
            model.start(request())
            testScheduler.advanceUntilIdle()
            player.finish()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("episode-1", "episode-2"), playback.sourceRequests)
        }
}
