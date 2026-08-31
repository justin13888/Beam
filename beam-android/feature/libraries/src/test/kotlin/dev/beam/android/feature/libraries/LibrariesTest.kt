package dev.beam.android.feature.libraries

import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.testing.FakeCatalogRepository
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
import uniffi.beam_client_core.BeamException

@OptIn(ExperimentalCoroutinesApi::class)
class LibrariesTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `libraries are listed`() =
        runTest {
            val viewModel = LibrariesViewModel(FakeCatalogRepository())
            testScheduler.advanceUntilIdle()

            assertTrue(
                viewModel.state.value.valueOrNull!!
                    .isNotEmpty(),
            )
        }

    @Test
    fun `a failure keeps what was already listed`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val viewModel = LibrariesViewModel(catalog)
            testScheduler.advanceUntilIdle()
            val loaded = viewModel.state.value.valueOrNull

            catalog.failWith = BeamException.Network("offline", retryable = true)
            viewModel.refresh()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state is LoadState.Failure)
            assertEquals(loaded, (state as LoadState.Failure).previous)
        }

    @Test
    fun `a scan that started and has not finished reads as in progress`() {
        // The pair of timestamps is the only signal the server gives; getting
        // this backwards would show a stale file count as though it were
        // final.
        val scanning =
            Fixtures.library().copy(
                lastScanStartedAtUnix = 1_700_000_000L,
                lastScanFinishedAtUnix = null,
            )
        assertTrue(scanning.isScanning)
        assertEquals("Scanning", scanning.summaryLine())
    }

    @Test
    fun `a finished scan does not read as in progress`() {
        val done =
            Fixtures.library().copy(
                lastScanStartedAtUnix = 1_700_000_000L,
                lastScanFinishedAtUnix = 1_700_000_060L,
            )
        assertFalse(done.isScanning)
    }

    @Test
    fun `a library that has never been scanned does not read as in progress`() {
        val fresh =
            Fixtures.library().copy(
                lastScanStartedAtUnix = null,
                lastScanFinishedAtUnix = null,
            )
        assertFalse(fresh.isScanning)
    }

    @Test
    fun `the file count is singular for one file and plural otherwise`() {
        val base =
            Fixtures.library().copy(
                lastScanStartedAtUnix = null,
                lastScanFinishedAtUnix = null,
            )
        assertEquals("Empty", base.copy(size = 0u).summaryLine())
        assertEquals("1 file", base.copy(size = 1u).summaryLine())
        assertEquals("128 files", base.copy(size = 128u).summaryLine())
    }
}
