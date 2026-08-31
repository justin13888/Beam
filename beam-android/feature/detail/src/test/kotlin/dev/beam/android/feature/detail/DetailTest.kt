package dev.beam.android.feature.detail

import androidx.lifecycle.SavedStateHandle
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.model.valueOrNull
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
    ) = DetailViewModel(
        catalog,
        playback,
        downloads,
        preferences,
        SavedStateHandle(mapOf("mediaId" to MEDIA_ID)),
    )

    @Test
    fun `a title loads with its sources`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            val state = model.state.value.valueOrNull
            assertNotNull(state)
            assertTrue(state!!.sources.isNotEmpty())
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
    fun `every quality preference maps to a policy`() {
        QualityPreference.entries.forEach { preference ->
            assertNotNull(preference.asPolicy())
        }
        assertEquals(QualityPolicy.Highest, QualityPreference.Best.asPolicy())
        assertEquals(QualityPolicy.MatchScreen, QualityPreference.MatchScreen.asPolicy())
        assertEquals(QualityPolicy.Smallest, QualityPreference.Smallest.asPolicy())
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

            model.download("file-9", "server-1", "Arrival", null)
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("file-9" to "Arrival"), downloads.enqueued)
        }

    private companion object {
        const val MEDIA_ID = "media-1"
    }
}
