package dev.beam.android.feature.history

import dev.beam.android.core.testing.FakePlaybackRepository
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
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.HistoryPage

@OptIn(ExperimentalCoroutinesApi::class)
class HistoryTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `history is listed`() =
        runTest {
            val viewModel = HistoryViewModel(FakePlaybackRepository())
            testScheduler.advanceUntilIdle()

            assertTrue(
                viewModel.state.value.entries
                    .isNotEmpty(),
            )
        }

    @Test
    fun `there is nothing more to fetch once every entry is loaded`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = HistoryPage(listOf(Fixtures.historyEntry()), total = 1uL)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            assertFalse(viewModel.state.value.hasMore)
        }

    @Test
    fun `a further page is requested from the right offset`() =
        runTest {
            // Offsets must count entries already held, not pages fetched: getting
            // this wrong silently skips or repeats entries.
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = HistoryPage(List(3) { Fixtures.historyEntry() }, total = 9uL)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()
            assertTrue(viewModel.state.value.hasMore)

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(6, viewModel.state.value.entries.size)
        }

    @Test
    fun `a failure is reported without discarding what is shown`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = HistoryPage(List(3) { Fixtures.historyEntry() }, total = 9uL)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()
            val loaded = viewModel.state.value.entries

            playback.failWith = BeamException.Network("offline", retryable = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.error)
            assertEquals(loaded, viewModel.state.value.entries)
        }

    @Test
    fun `a finished entry says so rather than showing time remaining`() {
        val entry = Fixtures.historyEntry().copy(completed = true)

        assertTrue(entry.statusLine().startsWith("Finished"))
    }

    @Test
    fun `an unfinished entry says how much is left`() {
        val entry =
            Fixtures.historyEntry().copy(
                completed = false,
                positionSecs = 600.0,
                durationSecs = 3_600.0,
            )

        assertFalse(entry.statusLine().startsWith("Finished"))
    }
}
