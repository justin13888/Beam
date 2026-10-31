package dev.beam.android.feature.explore

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
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.MediaPage
import uniffi.beam_client_core.MediaSortField
import uniffi.beam_client_core.MediaTypeFilter
import uniffi.beam_client_core.SortOrder

@OptIn(ExperimentalCoroutinesApi::class)
class ExploreViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `the first page is fetched exactly once on open`() =
        runTest {
            // The debounced search flow emits the initial empty query alongside the
            // explicit first load, so without dropping it the screen fetches page
            // one twice -- doubling the cost of every cold start.
            val catalog = FakeCatalogRepository()
            ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            assertEquals(1, catalog.browseCalls.size)
        }

    @Test
    fun `typing does not query on every keystroke`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()
            val before = catalog.browseCalls.size

            // Each keystroke lands just inside the debounce window of the one
            // before. Typed back to back with no time between them, the state
            // flow would conflate them into one query with no debounce at
            // all, and this test could not tell the difference.
            for (query in listOf("a", "ar", "arr", "arri")) {
                viewModel.onQueryChange(query)
                testScheduler.advanceTimeBy(KEYSTROKE_GAP_MS)
                testScheduler.runCurrent()
            }
            assertEquals("nothing is sent while the viewer is still typing", before, catalog.browseCalls.size)
            testScheduler.advanceUntilIdle()

            assertEquals(
                "a debounced field must issue one query, not one per character",
                before + 1,
                catalog.browseCalls.size,
            )
            assertEquals("arri", catalog.browseCalls.last().query)
        }

    @Test
    fun `changing a filter resets the cursor`() =
        runTest {
            // A Relay cursor is only meaningful within the query that produced it.
            // Reusing one across a filter change returns a page from the middle of
            // a different result set.
            val catalog = FakeCatalogRepository().apply { nextCursor = "cursor-1" }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            assertEquals("cursor-1", catalog.browseCalls.last().after)

            viewModel.onGenreChange("Science Fiction")
            testScheduler.advanceUntilIdle()

            assertNull(
                "a filter change must start from the beginning",
                catalog.browseCalls.last().after,
            )
            assertEquals("Science Fiction", catalog.browseCalls.last().genre)
        }

    @Test
    fun `a further page is appended rather than replacing what is shown`() =
        runTest {
            val catalog =
                FakeCatalogRepository().apply {
                    nextCursor = "cursor-1"
                    pages = listOf(pageOf("a", "b"), pageOf("c", "d"))
                }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("a", "b", "c", "d"),
                viewModel.state.value.items
                    .map { it.id },
            )
        }

    @Test
    fun `a title the shifting catalog repeats on a later page is listed once`() =
        runTest {
            // Indexing mutates the set underneath the cursor, so the server can
            // legitimately send a title again; listing it twice would put two
            // identical tiles in the grid.
            val catalog =
                FakeCatalogRepository().apply {
                    nextCursor = "cursor-1"
                    pages = listOf(pageOf("a", "b"), pageOf("b", "c"))
                }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("a", "b", "c"),
                viewModel.state.value.items
                    .map { it.id },
            )
        }

    @Test
    fun `a page that carries no cursor keeps the position the last one ended on`() =
        runTest {
            // Overwriting the cursor with nothing would send the next request
            // from the head, and the grid would fill with the first page again.
            val catalog =
                FakeCatalogRepository().apply {
                    pages =
                        listOf(
                            Fixtures.page(listOf(Fixtures.movie(id = "a")), endCursor = "cursor-1", hasNextPage = true),
                            Fixtures.page(listOf(Fixtures.movie(id = "b")), endCursor = null, hasNextPage = true),
                            pageOf("c"),
                        )
                }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf(null, "cursor-1", "cursor-1"), catalog.browseCalls.map { it.after })
            assertEquals(
                listOf("a", "b", "c"),
                viewModel.state.value.items
                    .map { it.id },
            )
        }

    @Test
    fun `load more does nothing when there is no next page`() =
        runTest {
            val catalog = FakeCatalogRepository().apply { nextCursor = null }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()
            val before = catalog.browseCalls.size

            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            assertEquals(before, catalog.browseCalls.size)
        }

    @Test
    fun `clearing filters removes every restriction at once`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.onQueryChange("arrival")
            viewModel.onGenreChange("Drama")
            viewModel.onMediaTypeChange(MediaTypeFilter.MOVIE)
            testScheduler.advanceUntilIdle()
            assertTrue(viewModel.state.value.hasFilters)

            viewModel.clearFilters()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(!state.hasFilters)
            val last = catalog.browseCalls.last()
            assertNull(last.query)
            assertNull(last.genre)
            assertNull(last.mediaType)
        }

    @Test
    fun `a blank query is sent as absent rather than as an empty string`() =
        runTest {
            // An empty `query` parameter is a filter that matches nothing on some
            // servers; omitting it is the only safe encoding of "no search".
            val catalog = FakeCatalogRepository()
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.onQueryChange("   ")
            testScheduler.advanceUntilIdle()

            assertNull(catalog.browseCalls.last().query)
        }

    @Test
    fun `a failure is reported without discarding what is already shown`() =
        runTest {
            val catalog = FakeCatalogRepository().apply { nextCursor = "cursor-1" }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()
            val loaded = viewModel.state.value.items

            catalog.failWith = BeamException.Network("offline", retryable = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertNotNull(state.error)
            assertEquals(
                "a failed page must not blank the results the viewer is reading",
                loaded,
                state.items,
            )
        }

    @Test
    fun `a failed page is asked for again from the same cursor`() =
        runTest {
            // A pager left waiting on the failed request would refuse every
            // later one, and the grid could never page on; one that advanced
            // past it would skip every title on that page.
            val catalog = FakeCatalogRepository().apply { nextCursor = "cursor-1" }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            catalog.failWith = BeamException.Network("offline", retryable = true)
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()
            catalog.failWith = null
            viewModel.loadMore()
            testScheduler.advanceUntilIdle()

            // The fake records only the requests it answered, so the failed
            // one is absent here.
            assertEquals(listOf(null, "cursor-1"), catalog.browseCalls.map { it.after })
        }

    @Test
    fun `selecting the sort field already in use reverses it`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            viewModel.onSortChange(MediaSortField.TITLE, SortOrder.DESCENDING)
            testScheduler.advanceUntilIdle()

            assertEquals(SortOrder.DESCENDING, catalog.browseCalls.last().sortOrder)
        }

    @Test
    fun `genres are loaded for the filter chips`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            assertTrue(
                viewModel.state.value.genres
                    .isNotEmpty(),
            )
        }

    @Test
    fun `a genre listing that fails does not stop the catalog loading`() =
        runTest {
            // The chips are a convenience; the grid is the screen. One failing must
            // not take the other with it.
            val catalog = FakeCatalogRepository().apply { genresFailWith = BeamException.Network("x", true) }
            val viewModel = ExploreViewModel(catalog)
            testScheduler.advanceUntilIdle()

            assertTrue(
                viewModel.state.value.genres
                    .isEmpty(),
            )
            assertTrue(
                viewModel.state.value.items
                    .isNotEmpty(),
            )
        }

    private companion object {
        /** One millisecond inside the view model's 300 ms search debounce. */
        const val KEYSTROKE_GAP_MS = 299L

        /** A last page of films with these ids. */
        fun pageOf(vararg ids: String): MediaPage = Fixtures.page(ids.map { Fixtures.movie(id = it) })
    }
}
