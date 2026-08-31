package dev.beam.android.feature.home

import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.MediaSortField
import uniffi.beam_client_core.SortOrder

@OptIn(ExperimentalCoroutinesApi::class)
class HomeViewModelTest {
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
    fun `every row is loaded`() =
        runTest {
            val viewModel = HomeViewModel(FakeCatalogRepository(), FakePlaybackRepository())
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value.valueOrNull
            assertNotNull(state)
            assertTrue(state!!.continueWatching.isNotEmpty())
            assertTrue(state.recentlyAdded.isNotEmpty())
            assertTrue(state.topRated.isNotEmpty())
        }

    @Test
    fun `the rows are ordered by what each one means`() =
        runTest {
            // Recently added by date descending, top rated by rating descending --
            // sorting a "top rated" row ascending would show the worst titles
            // first, which is never what the label promises.
            val catalog = FakeCatalogRepository()
            HomeViewModel(catalog, FakePlaybackRepository())
            testScheduler.advanceUntilIdle()

            val recent = catalog.browseCalls.first { it.sortBy == MediaSortField.DATE_ADDED }
            val top = catalog.browseCalls.first { it.sortBy == MediaSortField.RATING }
            assertEquals(SortOrder.DESCENDING, recent.sortOrder)
            assertEquals(SortOrder.DESCENDING, top.sortOrder)
        }

    @Test
    fun `the top rated row excludes unrated titles`() =
        runTest {
            // Without a floor, a "top rated" row fills with titles that have no
            // rating at all, which is worse than a shorter row.
            val catalog = FakeCatalogRepository()
            HomeViewModel(catalog, FakePlaybackRepository())
            testScheduler.advanceUntilIdle()

            val top = catalog.browseCalls.first { it.sortBy == MediaSortField.RATING }
            assertNotNull(top.minRating)
        }

    @Test
    fun `both rows are fetched, not one after the other by accident`() =
        runTest {
            val catalog = FakeCatalogRepository()
            HomeViewModel(catalog, FakePlaybackRepository())
            testScheduler.advanceUntilIdle()

            assertEquals(2, catalog.browseCalls.size)
        }

    @Test
    fun `a failure is reported as retryable when the cause is transient`() =
        runTest {
            val catalog =
                FakeCatalogRepository().apply {
                    failWith = BeamException.Network("offline", retryable = true)
                }
            val viewModel = HomeViewModel(catalog, FakePlaybackRepository())
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state is LoadState.Failure)
            assertTrue((state as LoadState.Failure).retryable)
        }

    @Test
    fun `refreshing keeps the rows on screen while it reloads`() =
        runTest {
            // Replacing populated rows with a spinner on every pull-to-refresh is
            // worse than a moment of slightly stale content.
            val catalog = FakeCatalogRepository()
            val viewModel = HomeViewModel(catalog, FakePlaybackRepository())
            testScheduler.advanceUntilIdle()
            val loaded = viewModel.state.value.valueOrNull

            viewModel.refresh()

            val duringRefresh = viewModel.state.value
            assertTrue(duringRefresh is LoadState.Loading)
            assertEquals(loaded, (duringRefresh as LoadState.Loading).previous)
        }

    @Test
    fun `a failed refresh keeps the rows that were already there`() =
        runTest {
            val catalog = FakeCatalogRepository()
            val viewModel = HomeViewModel(catalog, FakePlaybackRepository())
            testScheduler.advanceUntilIdle()
            val loaded = viewModel.state.value.valueOrNull

            catalog.failWith = BeamException.Network("offline", retryable = true)
            viewModel.refresh()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state is LoadState.Failure)
            assertEquals(loaded, (state as LoadState.Failure).previous)
        }
}
