package dev.beam.android.feature.downloads

import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.MediaDetail

/** Where Play on a download starts: the server's resume point, else the device's own. */
@OptIn(ExperimentalCoroutinesApi::class)
class DownloadsPlayTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    /** What playing [record] hands the player, through [catalog] and [playback]. */
    private fun TestScope.playing(
        record: DownloadRecord,
        catalog: CatalogRepository,
        playback: FakePlaybackRepository = FakePlaybackRepository(),
    ): PlaybackRequest? {
        val viewModel = DownloadsViewModel(FakeDownloadRepository(listOf(record)), catalog, playback)
        var request: PlaybackRequest? = null
        viewModel.play(record) { request = it }
        testScheduler.advanceUntilIdle()
        return request
    }

    /** A catalog that cannot be reached, as with no network. */
    private fun offline(): FakeCatalogRepository =
        FakeCatalogRepository().apply {
            failWith = BeamException.Network("offline", retryable = true)
        }

    /** A device that recorded the viewer at [positionSecs] into [fileId]. */
    private fun recorded(
        fileId: String,
        positionSecs: Double,
    ): FakePlaybackRepository = FakePlaybackRepository().apply { localStartPositions[fileId] = positionSecs }

    @Test
    fun `a second tap while the lookup is pending opens one player, not two`() =
        runTest {
            val record = downloadRecord(state = DownloadState.Completed)
            val viewModel =
                DownloadsViewModel(
                    FakeDownloadRepository(listOf(record)),
                    FakeCatalogRepository(),
                    FakePlaybackRepository(),
                )
            val requests = mutableListOf<PlaybackRequest>()

            viewModel.play(record) { requests += it }
            viewModel.play(record) { requests += it }
            testScheduler.advanceUntilIdle()
            assertEquals(1, requests.size)

            // Once the first has gone through, Play works again.
            viewModel.play(record) { requests += it }
            testScheduler.advanceUntilIdle()
            assertEquals(2, requests.size)
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

            // The server's answer wins over the device's own record of the file.
            val request = playing(record, catalog, recorded("a", 420.0))!!
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
    fun `a download played offline resumes from the position this device recorded`() =
        runTest {
            // Issue #487: the server cannot be asked, so the device's own
            // record of the file -- through the core's start rule -- is where
            // Play starts.
            val record = downloadRecord(fileId = "a", state = DownloadState.Completed)

            val request = playing(record, offline(), recorded("a", 420.0))
            assertEquals(420.0, request!!.startPositionSecs, 0.0)
        }

    @Test
    fun `a download played offline with nothing recorded starts at the beginning`() =
        runTest {
            val record = downloadRecord(fileId = "a", state = DownloadState.Completed)

            val request = playing(record, offline(), recorded("another-file", 420.0))
            assertEquals(0.0, request!!.startPositionSecs, 0.0)
        }

    @Test
    fun `a device record that cannot be read starts at the beginning rather than failing`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    localStartPositions["a"] = 420.0
                    localStartFailure = BeamException.Storage("locked")
                }
            val record = downloadRecord(fileId = "a", state = DownloadState.Completed)

            val request = playing(record, offline(), playback)
            assertEquals(0.0, request!!.startPositionSecs, 0.0)
        }

    @Test
    fun `a server that never answers does not hold the player back`() =
        runTest {
            // A half-open connection hangs rather than failing; the lookup is
            // bounded so the downloaded file still plays, from where this
            // device last recorded it.
            val hanging =
                object : CatalogRepository by FakeCatalogRepository() {
                    override suspend fun detail(mediaId: String): MediaDetail = awaitCancellation()
                }
            val record = downloadRecord(fileId = "a", state = DownloadState.Completed)

            val request = playing(record, hanging, recorded("a", 420.0))
            assertEquals(420.0, request!!.startPositionSecs, 0.0)
        }
}
