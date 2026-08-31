package dev.beam.android.feature.downloads

import dev.beam.android.core.model.DownloadState
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class DownloadsTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    /**
     * Start collecting, because the screen's state is shared
     * `WhileSubscribed`.
     *
     * That is deliberate in production -- an unobserved downloads tab should
     * not hold a listener on the download manager -- but it means a test that
     * only reads `.value` observes the initial value forever.
     */
    private fun CoroutineScope.observe(viewModel: DownloadsViewModel) {
        launch { viewModel.state.collect {} }
    }

    @Test
    fun `downloads are grouped by what can be done with them`() =
        runTest {
            val repository =
                FakeDownloadRepository(
                    listOf(
                        downloadRecord(fileId = "a", state = DownloadState.Completed),
                        downloadRecord(fileId = "b", state = DownloadState.Downloading),
                        downloadRecord(fileId = "c", state = DownloadState.Paused),
                        downloadRecord(fileId = "d", state = DownloadState.WaitingForNetwork),
                        downloadRecord(fileId = "e", state = DownloadState.Queued),
                        downloadRecord(fileId = "f", state = DownloadState.Failed),
                    ),
                )
            val viewModel = DownloadsViewModel(repository)
            backgroundScope.observe(viewModel)
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(1, state.completed.size)
            assertEquals(
                "queued, downloading, paused and waiting all read as in progress",
                4,
                state.inProgress.size,
            )
            assertEquals(1, state.failed.size)
        }

    @Test
    fun `an empty store renders as empty rather than as an error`() =
        runTest {
            val viewModel = DownloadsViewModel(FakeDownloadRepository())
            backgroundScope.observe(viewModel)
            testScheduler.advanceUntilIdle()

            assertTrue(viewModel.state.value.isEmpty)
        }

    @Test
    fun `pausing and resuming move a download between groups`() =
        runTest {
            val repository =
                FakeDownloadRepository(
                    listOf(downloadRecord(fileId = "a", state = DownloadState.Downloading)),
                )
            val viewModel = DownloadsViewModel(repository)
            backgroundScope.observe(viewModel)
            testScheduler.advanceUntilIdle()

            viewModel.pause("a")
            testScheduler.advanceUntilIdle()
            assertEquals(
                DownloadState.Paused,
                viewModel.state.value.inProgress
                    .single()
                    .state,
            )

            viewModel.resume("a")
            testScheduler.advanceUntilIdle()
            assertEquals(
                DownloadState.Downloading,
                viewModel.state.value.inProgress
                    .single()
                    .state,
            )
        }

    @Test
    fun `removing a download takes it out of the list`() =
        runTest {
            val repository =
                FakeDownloadRepository(
                    listOf(downloadRecord(fileId = "a", state = DownloadState.Completed)),
                )
            val viewModel = DownloadsViewModel(repository)
            backgroundScope.observe(viewModel)
            testScheduler.advanceUntilIdle()

            viewModel.remove("a")
            testScheduler.advanceUntilIdle()

            assertTrue(viewModel.state.value.isEmpty)
        }

    @Test
    fun `a store that cannot be opened renders empty rather than crashing`() =
        runTest {
            // The download manager needs a session to build, so opening the tab
            // before signing in must not take the app down.
            val repository =
                FakeDownloadRepository().apply {
                    failWith = IllegalStateException("no session yet")
                }
            val viewModel = DownloadsViewModel(repository)
            backgroundScope.observe(viewModel)
            testScheduler.advanceUntilIdle()

            assertTrue(viewModel.state.value.isEmpty)
        }

    @Test
    fun `the status line shows progress against the total once it is known`() {
        val record =
            downloadRecord(
                state = DownloadState.Downloading,
                downloadedBytes = 500L * 1024 * 1024,
                totalBytes = 4L * 1024 * 1024 * 1024,
            )

        assertTrue(record.statusLine().contains(" of "))
    }

    @Test
    fun `the status line does not invent a total it does not have`() {
        val record =
            downloadRecord(
                state = DownloadState.Downloading,
                downloadedBytes = 1024L,
                totalBytes = 0L,
            )

        assertTrue(!record.statusLine().contains(" of "))
    }

    @Test
    fun `a failed download explains that retrying resumes rather than restarts`() {
        val record =
            downloadRecord(state = DownloadState.Failed).copy(
                failureMessage = "The download stopped. It will resume when you retry it.",
            )

        assertTrue(record.statusLine().contains("resume"))
    }
}
