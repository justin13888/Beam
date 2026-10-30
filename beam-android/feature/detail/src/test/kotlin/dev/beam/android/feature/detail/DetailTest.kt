package dev.beam.android.feature.detail

import androidx.lifecycle.SavedStateHandle
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.Playability
import uniffi.beam_client_core.QualityPolicy
import uniffi.beam_client_core.SourceSelection

@OptIn(ExperimentalCoroutinesApi::class)
class DetailTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun catalogWith(mediaId: String = MEDIA_ID) =
        FakeCatalogRepository().apply {
            details[mediaId] = Fixtures.showDetail()
        }

    private fun viewModel(
        catalog: FakeCatalogRepository = catalogWith(),
        playback: FakePlaybackRepository = FakePlaybackRepository(),
        downloads: FakeDownloadRepository = FakeDownloadRepository(),
        preferences: FakePreferencesRepository = FakePreferencesRepository(),
        // Already restored: a detail page is reached only past sign-in.
        servers: FakeServerRepository = FakeServerRepository(listOf(Fixtures.server()), restored = true),
    ) = DetailViewModel(
        catalog,
        playback,
        downloads,
        preferences,
        servers,
        SavedStateHandle(mapOf("mediaId" to MEDIA_ID)),
    )

    @Test
    fun `a title loads with its sources`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    sourceList = listOf(Fixtures.source(fileId = "uhd"), Fixtures.source(fileId = "hd", height = 1080u))
                }
            val model = viewModel(playback = playback)
            testScheduler.advanceUntilIdle()

            val state = model.state.value.valueOrNull
            assertNotNull(state)
            assertEquals(listOf(MEDIA_ID), playback.sourceRequests)
            assertEquals("the server's sources, in its order", listOf("uhd", "hd"), state!!.sources.map { it.fileId })
        }

    @Test
    fun `the first season is selected so the episode list is never empty`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            val state = model.state.value.valueOrNull!!
            assertNotNull(state.selectedSeason)
            assertTrue(state.episodes.isNotEmpty())
        }

    @Test
    fun `the viewer's quality preference chooses the source`() =
        runTest {
            // The mapping from the settings screen's words to the core's policies
            // is a product decision, and getting it wrong silently plays the wrong
            // file on every title.
            val playback = FakePlaybackRepository()
            viewModel(
                playback = playback,
                preferences =
                    FakePreferencesRepository(
                        UserPreferences(quality = QualityPreference.Smallest),
                    ),
            )
            testScheduler.advanceUntilIdle()

            assertEquals(QualityPolicy.Smallest, playback.selectionPolicy)
        }

    @Test
    fun `a title whose sources cannot be listed still renders`() =
        runTest {
            // A page that shows the synopsis and says "unavailable" is more useful
            // than an error page with nothing on it.
            val playback =
                FakePlaybackRepository().apply {
                    failWith = BeamException.Network("offline", retryable = true)
                }
            val model = viewModel(playback = playback)
            testScheduler.advanceUntilIdle()

            val state = model.state.value.valueOrNull
            assertNotNull(state)
            assertTrue(state!!.sources.isEmpty())
            assertNull(state.selection)
        }

    @Test
    fun `a title that cannot be loaded at all is a failure`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val model = viewModel(catalog = catalog)
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value is LoadState.Failure)
        }

    @Test
    fun `software-only playback is surfaced so the viewer can choose otherwise`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    selection =
                        SourceSelection(
                            source = Fixtures.source(),
                            playability = Playability.Software("no hardware HEVC decoder"),
                            audioTrackIndex = null,
                            reason = "Only a software decoder is available",
                            rejected = emptyList(),
                        )
                }
            val model = viewModel(playback = playback)
            testScheduler.advanceUntilIdle()

            assertTrue(
                model.state.value.valueOrNull!!
                    .isSoftwareOnly,
            )
        }

    @Test
    fun `hardware playback is not flagged`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    selection =
                        SourceSelection(
                            source = Fixtures.source(),
                            playability = Playability.Hardware,
                            audioTrackIndex = null,
                            reason = "",
                            rejected = emptyList(),
                        )
                }
            val model = viewModel(playback = playback)
            testScheduler.advanceUntilIdle()

            assertTrue(
                !model.state.value.valueOrNull!!
                    .isSoftwareOnly,
            )
        }

    @Test
    fun `choosing a season changes the episodes without refetching`() =
        runTest {
            val catalog = catalogWith()
            val model = viewModel(catalog = catalog)
            testScheduler.advanceUntilIdle()
            val detailCallsBefore = catalog.browseCalls.size

            model.selectSeason(2u)
            testScheduler.advanceUntilIdle()

            assertEquals(
                2u,
                model.state.value.valueOrNull!!
                    .selectedSeason,
            )
            assertEquals(detailCallsBefore, catalog.browseCalls.size)
        }

    @Test
    fun `the source picker opens and closes`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            model.setPickingSource(true)
            assertTrue(
                model.state.value.valueOrNull!!
                    .isPickingSource,
            )

            model.setPickingSource(false)
            assertTrue(
                !model.state.value.valueOrNull!!
                    .isPickingSource,
            )
        }

    @Test
    fun `downloading queues the chosen file`() =
        runTest {
            val downloads = FakeDownloadRepository()
            val model = viewModel(downloads = downloads)
            testScheduler.advanceUntilIdle()

            model.download("file-9", "Arrival")
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-9" to "Arrival"), downloads.enqueued)
        }

    @Test
    fun `downloading with no server signed in explains itself`() =
        runTest {
            val downloads = FakeDownloadRepository()
            val model = viewModel(downloads = downloads, servers = FakeServerRepository(emptyList()))
            testScheduler.advanceUntilIdle()

            model.download("file-9", "Arrival")
            testScheduler.advanceUntilIdle()

            assertTrue(downloads.enqueued.isEmpty())
            assertNotNull(
                "a silent tap that does nothing reads as a broken button",
                model.state.value.valueOrNull
                    ?.message,
            )
        }

    @Test
    fun `a started download tells the viewer it started`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            model.download("file-9", "Arrival")
            testScheduler.advanceUntilIdle()

            assertNotNull(
                model.state.value.valueOrNull
                    ?.message,
            )
        }

    @Test
    fun `a message is cleared once it has been shown`() =
        runTest {
            // Otherwise a configuration change replays a confirmation the viewer
            // has already read.
            val model = viewModel()
            testScheduler.advanceUntilIdle()
            model.download("file-9", "Arrival")
            testScheduler.advanceUntilIdle()

            model.clearMessage()

            assertNull(
                model.state.value.valueOrNull
                    ?.message,
            )
        }

    /** A two-season show, its second season's episode in the given state. */
    private fun twoSeasons(secondWatched: Boolean) =
        Fixtures.showDetail(
            seasons =
                listOf(
                    Fixtures.season(number = 1u, episodes = listOf(Fixtures.episode(id = "s1e1"))),
                    Fixtures.season(
                        number = 2u,
                        episodes = listOf(Fixtures.episode(id = "s2e1", played = secondWatched)),
                    ),
                ),
        )

    @Test
    fun `marking an episode watched tells the server and shows what it now says`() =
        runTest {
            val catalog = FakeCatalogRepository().apply { details[MEDIA_ID] = twoSeasons(secondWatched = false) }
            val playback = FakePlaybackRepository()
            val model = viewModel(catalog = catalog, playback = playback)
            testScheduler.advanceUntilIdle()
            model.selectSeason(2u)
            // What the server answers once the mark has landed.
            catalog.details[MEDIA_ID] = twoSeasons(secondWatched = true)

            model.setWatched("s2e1", watched = true)
            testScheduler.advanceUntilIdle()

            assertEquals(setOf("s2e1"), playback.watched)
            val state = model.state.value.valueOrNull!!
            assertTrue("the title is read back after the mark", state.episodes.single().played)
            assertEquals("the season on screen stays on screen", 2u, state.selectedSeason)
        }

    @Test
    fun `marking unwatched takes the mark back`() =
        runTest {
            val playback = FakePlaybackRepository().apply { watched += "s2e1" }
            val model = viewModel(playback = playback)
            testScheduler.advanceUntilIdle()

            model.setWatched("s2e1", watched = false)
            testScheduler.advanceUntilIdle()

            assertTrue(playback.watched.isEmpty())
        }

    @Test
    fun `a mark the server refuses says so and keeps the page`() =
        runTest {
            val playback = FakePlaybackRepository()
            val model = viewModel(playback = playback)
            testScheduler.advanceUntilIdle()
            playback.failWith = BeamException.Network("offline", true)

            model.setWatched("episode-1", watched = true)
            testScheduler.advanceUntilIdle()

            val state = model.state.value
            assertTrue("the page is still there", state is LoadState.Success)
            assertNotNull(state.valueOrNull?.message)
            assertTrue(playback.watched.isEmpty())
        }

    @Test
    fun `how much of a title is watched reads as words`() {
        fun label(summary: uniffi.beam_client_core.MediaSummary) =
            watchedLabel(DetailUiState(detail = Fixtures.movieDetail(summary)))

        assertEquals("Watched", label(Fixtures.movie(played = true)))
        assertNull("nothing to say about a title not started", label(Fixtures.movie()))
        assertEquals(
            "3 of 12 watched",
            label(Fixtures.show(episodeCount = 12u).copy(watchedEpisodeCount = 3u)),
        )
    }

    private companion object {
        const val MEDIA_ID = "media-1"
    }
}
