package dev.beam.android.feature.detail

import androidx.lifecycle.SavedStateHandle
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.Playability
import uniffi.beam_client_core.SourceSelection

/** Which of a title's files the page treats as downloaded, and so offers to play offline. */
@OptIn(ExperimentalCoroutinesApi::class)
class DetailDownloadsTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `a completed download of the chosen file plays offline`() =
        runTest {
            val downloads = FakeDownloadRepository().apply { completed += CHOSEN }

            val state = loaded(downloads)

            assertTrue(state.playsOffline)
            assertEquals(setOf(CHOSEN), state.downloadedFileIds)
        }

    @Test
    fun `a partial download does not show Play offline`() =
        runTest {
            // Queued, but not complete: the index says so, whatever it holds.
            val downloads = FakeDownloadRepository()
            downloads.enqueue(CHOSEN, "home", MEDIA_ID, "Arrival")

            val state = loaded(downloads)

            assertTrue("the page asked about the chosen file", CHOSEN in downloads.asked)
            assertFalse(state.playsOffline)
            assertTrue(state.downloadedFileIds.isEmpty())
        }

    @Test
    fun `another source downloaded in full does not make the chosen one offline`() =
        runTest {
            val downloads = FakeDownloadRepository().apply { completed += OTHER }

            val state = loaded(downloads)

            assertEquals("only the chosen file is looked up", setOf(CHOSEN), downloads.asked)
            assertTrue(state.downloadedFileIds.isEmpty())
            assertFalse(state.playsOffline)
        }

    /** A film's page with two sources, [CHOSEN] the one Play plays, over [downloads]. */
    private fun TestScope.loaded(downloads: FakeDownloadRepository): DetailUiState {
        val catalog = FakeCatalogRepository().apply { details[MEDIA_ID] = Fixtures.movieDetail() }
        val playback =
            FakePlaybackRepository().apply {
                sourceList = listOf(Fixtures.source(fileId = CHOSEN), Fixtures.source(fileId = OTHER))
                selection = selection()
            }
        val model =
            DetailViewModel(
                catalog,
                playback,
                downloads,
                FakePreferencesRepository(),
                FakeServerRepository(listOf(Fixtures.server()), restored = true),
                SavedStateHandle(mapOf("mediaId" to MEDIA_ID)),
            )
        testScheduler.advanceUntilIdle()
        return model.state.value.valueOrNull!!
    }

    private fun selection() =
        SourceSelection(
            source = Fixtures.source(fileId = CHOSEN),
            playability = Playability.Hardware,
            audioTrackIndex = null,
            reason = "",
            rejected = emptyList(),
        )

    private companion object {
        const val MEDIA_ID = "movie-1"
        const val CHOSEN = "uhd"
        const val OTHER = "hd"
    }
}
