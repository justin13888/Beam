package dev.beam.android.feature.history

import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.CompletableDeferred
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
import uniffi.beam_client_core.MediaKind

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
            val playback =
                FakePlaybackRepository().apply {
                    historyPage =
                        HistoryPage(
                            listOf(
                                Fixtures.historyEntry(mediaId = "newest"),
                                Fixtures.historyEntry(mediaId = "older", completed = false),
                            ),
                            2uL,
                            endCursor = "c1",
                            hasNextPage = false,
                        )
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            assertEquals(
                "the server's rows, in the server's order",
                listOf("newest", "older"),
                viewModel.state.value.entries
                    .map { it.mediaId },
            )
        }

    @Test
    fun `there is nothing more to fetch once every entry is loaded`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    historyPage =
                        HistoryPage(
                            listOf(Fixtures.historyEntry()),
                            total = 1uL,
                            endCursor = "c1",
                            hasNextPage = false,
                        )
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            assertFalse(viewModel.state.value.hasMore)
        }

    @Test
    fun `a further page is requested after the cursor the last one ended on`() =
        runTest {
            // Paging on from anything but the last page's end cursor silently
            // skips or repeats entries.
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", "b", "c", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()
            assertTrue(viewModel.state.value.hasMore)

            playback.historyPage = pageOf("d", "e", "f", endCursor = "page-2", hasNextPage = false)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("a", "b", "c", "d", "e", "f"),
                viewModel.state.value.entries
                    .map { it.mediaId },
            )
            assertEquals(listOf(null, "page-1"), playback.historyAfters)
        }

    @Test
    fun `a title played again while paging is listed once`() =
        runTest {
            // Playing something moves it to the head of the history, so the
            // next page can carry a title the first already showed.
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", "b", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            playback.historyPage = pageOf("b", "c", endCursor = "page-2", hasNextPage = false)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("a", "b", "c"),
                viewModel.state.value.entries
                    .map { it.mediaId },
            )
        }

    @Test
    fun `two episodes of one series on different pages are both listed`() =
        runTest {
            // Episodes share their series' media id, so a row is told apart by
            // its episode; keyed on the series alone, the second episode would
            // be dropped as a repeat of the first.
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = episodePage("episode-2", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            playback.historyPage = episodePage("episode-1", endCursor = "page-2", hasNextPage = false)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("episode-2", "episode-1"),
                viewModel.state.value.entries
                    .map { it.episodeId },
            )
        }

    @Test
    fun `a page that carries no cursor keeps the position the last one ended on`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            playback.historyPage = pageOf("b", endCursor = null, hasNextPage = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf(null, "page-1", "page-1"), playback.historyAfters)
        }

    @Test
    fun `refreshing starts again from the head`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            viewModel.refresh()
            testScheduler.advanceUntilIdle()

            assertEquals(null, playback.historyAfters.last())
            assertEquals(
                "a refreshed list has forgotten what it showed, so its first row is listed",
                listOf("a"),
                viewModel.state.value.entries
                    .map { it.mediaId },
            )
        }

    @Test
    fun `a further page still arriving when the list is refreshed is dropped`() =
        runTest {
            // The page belongs to the list being replaced: landing it on the
            // reset pager would append stale rows and page on from its cursor.
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            val stale = CompletableDeferred<Unit>()
            playback.historyHold = stale
            playback.historyPage = pageOf("stale", endCursor = "stale-cursor", hasNextPage = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            playback.historyHold = null
            playback.historyPage = pageOf("fresh", endCursor = "fresh-cursor", hasNextPage = true)
            viewModel.refresh()
            testScheduler.advanceUntilIdle()
            stale.complete(Unit)
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("fresh"),
                viewModel.state.value.entries
                    .map { it.mediaId },
            )
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            assertEquals("fresh-cursor", playback.historyAfters.last())
        }

    @Test
    fun `a failure is reported without discarding what is shown`() =
        runTest {
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", "b", "c", endCursor = "page-1", hasNextPage = true)
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

    @Test
    fun `a failed page is asked for again from the same cursor`() =
        runTest {
            // Advancing past a page that never arrived would skip every entry
            // on it.
            val playback =
                FakePlaybackRepository().apply {
                    historyPage = pageOf("a", endCursor = "page-1", hasNextPage = true)
                }
            val viewModel = HistoryViewModel(playback)
            testScheduler.advanceUntilIdle()

            playback.failWith = BeamException.Network("offline", retryable = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            playback.failWith = null
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf(null, "page-1", "page-1"), playback.historyAfters)
        }

    private fun pageOf(
        vararg mediaIds: String,
        endCursor: String?,
        hasNextPage: Boolean,
    ): HistoryPage =
        HistoryPage(
            mediaIds.map { Fixtures.historyEntry(mediaId = it) },
            total = 9uL,
            endCursor = endCursor,
            hasNextPage = hasNextPage,
        )

    /** A page holding one episode of the series `show-1`. */
    private fun episodePage(
        episodeId: String,
        endCursor: String?,
        hasNextPage: Boolean,
    ): HistoryPage =
        HistoryPage(
            listOf(
                Fixtures
                    .historyEntry(mediaId = "show-1")
                    .copy(episodeId = episodeId, kind = MediaKind.SHOW),
            ),
            total = 9uL,
            endCursor = endCursor,
            hasNextPage = hasNextPage,
        )
}
