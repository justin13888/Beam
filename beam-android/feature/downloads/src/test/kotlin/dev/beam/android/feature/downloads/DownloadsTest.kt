package dev.beam.android.feature.downloads

import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.awaitCancellation
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
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.MediaDetail

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
            val viewModel = DownloadsViewModel(repository, FakeCatalogRepository())
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
            val viewModel = DownloadsViewModel(FakeDownloadRepository(), FakeCatalogRepository())
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
            val viewModel = DownloadsViewModel(repository, FakeCatalogRepository())
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
            val viewModel = DownloadsViewModel(repository, FakeCatalogRepository())
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
            val viewModel = DownloadsViewModel(repository, FakeCatalogRepository())
            backgroundScope.observe(viewModel)
            testScheduler.advanceUntilIdle()

            assertTrue(viewModel.state.value.isEmpty)
        }

    /** What playing [record] hands the player, through [catalog]. */
    private fun TestScope.playing(
        record: DownloadRecord,
        catalog: CatalogRepository,
    ): PlaybackRequest? {
        val viewModel = DownloadsViewModel(FakeDownloadRepository(listOf(record)), catalog)
        var request: PlaybackRequest? = null
        viewModel.play(record) { request = it }
        testScheduler.advanceUntilIdle()
        return request
    }

    @Test
    fun `a downloaded film resumes where the core says it starts`() =
        runTest {
            // The core's start rather than the raw resume point (issue #466).
            val catalog =
                FakeCatalogRepository().apply {
                    details["media-1"] =
                        Fixtures.movieDetail(
                            Fixtures
                                .movie(
                                    id = "media-1",
                                ).copy(resumePositionSecs = 6_250.0, startPositionSecs = 1_200.0),
                        )
                }
            val record = downloadRecord(fileId = "a", state = DownloadState.Completed)

            val request = playing(record, catalog)!!
            assertEquals(1_200.0, request.startPositionSecs, 0.0)
            assertEquals("a", request.fileId)
            assertEquals("media-1", request.mediaId)
        }

    @Test
    fun `a downloaded episode resumes from its own start, not the show's`() =
        runTest {
            val catalog =
                FakeCatalogRepository().apply {
                    details["media-1"] =
                        Fixtures.showDetail(
                            seasons =
                                listOf(
                                    Fixtures.season(
                                        episodes =
                                            listOf(
                                                Fixtures.episode(id = "e1").copy(startPositionSecs = 300.0),
                                                Fixtures.episode(id = "e2").copy(startPositionSecs = 900.0),
                                            ),
                                    ),
                                ),
                        )
                }
            val record = downloadRecord(state = DownloadState.Completed).copy(episodeId = "e2")

            val request = playing(record, catalog)!!
            assertEquals(900.0, request.startPositionSecs, 0.0)
            assertEquals("e2", request.episodeId)
        }

    @Test
    fun `a download played offline starts at the beginning rather than failing`() =
        runTest {
            val catalog =
                FakeCatalogRepository().apply {
                    failWith = BeamException.Network("offline", retryable = true)
                }

            val request = playing(downloadRecord(state = DownloadState.Completed), catalog)
            assertEquals(0.0, request!!.startPositionSecs, 0.0)
        }

    @Test
    fun `a server that never answers does not hold the player back`() =
        runTest {
            // A half-open connection hangs rather than failing; the lookup is
            // bounded so the downloaded file still plays.
            val hanging =
                object : CatalogRepository by FakeCatalogRepository() {
                    override suspend fun detail(mediaId: String): MediaDetail = awaitCancellation()
                }

            val request = playing(downloadRecord(state = DownloadState.Completed), hanging)
            assertEquals(0.0, request!!.startPositionSecs, 0.0)
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
