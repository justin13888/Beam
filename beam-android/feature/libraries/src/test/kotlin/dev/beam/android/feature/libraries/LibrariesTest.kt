package dev.beam.android.feature.libraries

import androidx.lifecycle.SavedStateHandle
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
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.FileIndexStatus

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

/** The library detail screen, which lists a library's indexed files. */
@OptIn(ExperimentalCoroutinesApi::class)
class LibraryDetailTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun viewModel(catalog: FakeCatalogRepository = catalogWithFiles()) =
        LibraryDetailViewModel(catalog, SavedStateHandle(mapOf("libraryId" to LIBRARY_ID)))

    private fun catalogWithFiles() =
        FakeCatalogRepository().apply {
            libraryList = listOf(Fixtures.library(id = LIBRARY_ID))
            files = listOf(Fixtures.libraryFile(), Fixtures.libraryFile(id = "file-2"))
        }

    @Test
    fun `a library loads with its files`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            val state = model.state.value.valueOrNull
            assertNotNull(state)
            assertEquals(2, state!!.files.size)
        }

    @Test
    fun `a library whose files cannot be read still names the library`() =
        runTest {
            // The file listing failing is not the same as the library being gone;
            // an error page with nothing on it would be worse than a short one.
            val catalog =
                catalogWithFiles().apply {
                    filesFailWith = BeamException.Network("offline", retryable = true)
                }
            val model = viewModel(catalog)
            testScheduler.advanceUntilIdle()

            val state = model.state.value.valueOrNull
            assertNotNull(state)
            assertTrue(state!!.files.isEmpty())
            assertEquals(LIBRARY_ID, state.library.id)
        }

    @Test
    fun `a library that cannot be loaded at all is a failure`() =
        runTest {
            val catalog = FakeCatalogRepository().apply { libraryList = emptyList() }
            val model = viewModel(catalog)
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value is LoadState.Failure)
        }

    @Test
    fun `a changed file says so rather than looking indexed`() {
        // A changed or unscanned file is the usual reason a title will not
        // play, and an operator cannot act on what the screen does not say.
        val changed = Fixtures.libraryFile().copy(status = FileIndexStatus.CHANGED)
        assertTrue(changed.detailLine().contains("Changed"))

        val known = Fixtures.libraryFile().copy(status = FileIndexStatus.KNOWN)
        assertFalse(known.detailLine().contains("Changed"))
    }

    private companion object {
        const val LIBRARY_ID = "library-1"
    }
}
