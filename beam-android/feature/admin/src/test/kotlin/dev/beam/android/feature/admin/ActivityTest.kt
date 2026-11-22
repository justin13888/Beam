package dev.beam.android.feature.admin

import dev.beam.android.core.model.LoadState
import dev.beam.android.core.testing.AdminFixtures
import dev.beam.android.core.testing.FakeAdminRepository
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
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException

/** The log viewer, the event feed and the library telemetry preview. */
@OptIn(ExperimentalCoroutinesApi::class)
class ActivityTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun logLines(count: Int) = (1..count).map { AdminFixtures.logEntry(id = "log-$it", message = "line $it") }

    @Test
    fun `the log reads a page at a time until the server has no more`() =
        runTest {
            val lines = logLines(75)
            val admin = FakeAdminRepository().apply { logEntries = lines }
            val viewModel = AdminLogsViewModel(admin)
            testScheduler.advanceUntilIdle()

            val first = viewModel.state.value
            assertEquals(lines.take(50), first.items)
            assertEquals(75uL, first.total)
            assertTrue(first.hasNextPage)

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            val all = viewModel.state.value
            assertEquals("every line, once, in the server's order", lines, all.items)
            assertFalse(all.hasNextPage)

            // Nothing follows the last page, so asking again reads nothing.
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            assertEquals(lines, viewModel.state.value.items)
        }

    @Test
    fun `a page of the log that fails keeps the lines already read and says why`() =
        runTest {
            val admin = FakeAdminRepository().apply { logEntries = logLines(60) }
            val viewModel = AdminLogsViewModel(admin)
            testScheduler.advanceUntilIdle()

            admin.failWith = BeamException.Network("connection reset", retryable = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(50, state.items.size)
            assertNotNull(state.error)
            assertFalse(state.isLoadingMore)

            // The failed page is asked for again, from where the list stopped.
            admin.failWith = null
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            assertEquals(60, viewModel.state.value.items.size)
            assertNull(viewModel.state.value.error)
        }

    @Test
    fun `a non-administrator's log is the forbidden failure, not an empty log`() =
        runTest {
            val admin = FakeAdminRepository().apply { failWith = BeamException.Forbidden("admin only", "about:blank") }
            val viewModel = AdminLogsViewModel(admin)
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.items.isEmpty())
            assertNotNull(state.error)
        }

    @Test
    fun `the event feed shows the most recent events and keeps them when a refresh fails`() =
        runTest {
            val events = listOf(AdminFixtures.event(id = "e2", message = "newer"), AdminFixtures.event(id = "e1"))
            val admin = FakeAdminRepository().apply { eventEntries = events }
            val viewModel = AdminEventsViewModel(admin)
            testScheduler.advanceUntilIdle()
            assertEquals(events, viewModel.state.value.events)

            admin.failWith = BeamException.Network("down", retryable = true)
            viewModel.refresh()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals("the feed is not blanked", events, state.events)
            assertNotNull(state.error)
            assertFalse(state.isLoading)
        }

    @Test
    fun `the library telemetry preview loads, and a failed refresh keeps the report on screen`() =
        runTest {
            val admin = FakeAdminRepository()
            val viewModel = LibraryTelemetryViewModel(admin)
            testScheduler.advanceUntilIdle()
            assertEquals(LoadState.Success(admin.libraryPreview), viewModel.state.value)

            admin.failWith = BeamException.Network("down", retryable = true)
            viewModel.refresh()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state is LoadState.Failure)
            assertEquals(admin.libraryPreview, (state as LoadState.Failure).previous)
            assertTrue(state.retryable)
        }

    @Test
    fun `a preview with no collector says nothing is sent`() {
        val off = AdminFixtures.libraryTelemetryPreview(destinationConfigured = false)
        assertTrue(off.destinationSummary().startsWith("Not sending"))
        val on = AdminFixtures.libraryTelemetryPreview(destinationConfigured = true)
        assertTrue(on.destinationSummary().contains("https://collector.example"))
    }
}
