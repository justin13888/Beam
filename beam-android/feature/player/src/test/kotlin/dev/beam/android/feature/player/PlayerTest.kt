package dev.beam.android.feature.player

import androidx.lifecycle.SavedStateHandle
import dev.beam.android.core.media.session.PlayerProvider
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
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
import uniffi.beam_client_core.Playability
import uniffi.beam_client_core.SourceSelection

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
        playback: FakePlaybackRepository = FakePlaybackRepository(),
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
        playback = playback,
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

    private fun smallerSource() =
        SourceSelection(
            source = Fixtures.source(fileId = "file-sd", width = 854u, height = 480u),
            playability = Playability.Hardware,
            audioTrackIndex = null,
            reason = DOWNGRADE_REASON,
            rejected = emptyList(),
        )

    private fun request() =
        PlaybackRequest(
            mediaId = "media-1",
            episodeId = "episode-1",
            fileId = "file-1",
            title = "Arrival",
        )

    private companion object {
        const val DOWNGRADE_REASON = "Switched to 480p because playback kept stalling"
    }
}
