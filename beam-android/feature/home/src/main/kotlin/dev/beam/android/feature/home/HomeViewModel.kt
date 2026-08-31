package dev.beam.android.feature.home

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.model.LoadState
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.BrowseQuery
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.MediaSortField
import uniffi.beam_client_core.MediaSummary
import uniffi.beam_client_core.SortOrder
import javax.inject.Inject

/** What the home screen shows. */
public data class HomeUiState(
    /** Partly-watched titles, ready to resume. */
    val continueWatching: List<ContinueWatchingEntry> = emptyList(),
    /** Most recently added titles. */
    val recentlyAdded: List<MediaSummary> = emptyList(),
    /** Highest-rated titles. */
    val topRated: List<MediaSummary> = emptyList(),
) {
    /** Whether there is nothing at all to show. */
    public val isEmpty: Boolean
        get() = continueWatching.isEmpty() && recentlyAdded.isEmpty() && topRated.isEmpty()
}

/**
 * The home screen's rows.
 *
 * The three rows are fetched concurrently rather than in sequence: they are
 * independent, and on a home connection three sequential round trips is the
 * difference between the screen appearing at once and appearing in stages.
 */
@HiltViewModel
public class HomeViewModel
    @Inject
    constructor(
        private val catalog: CatalogRepository,
        private val playback: PlaybackRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow<LoadState<HomeUiState>>(LoadState.Idle)
        public val state: StateFlow<LoadState<HomeUiState>> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Reload every row. */
        public fun refresh() {
            // The previous value is carried into Loading so a pull-to-refresh
            // shows the existing rows with a spinner over them, rather than
            // blanking the screen the viewer was already reading.
            mutableState.update { LoadState.Loading(it.previousValue()) }
            viewModelScope.launch {
                try {
                    val loaded =
                        coroutineScope {
                            val resuming = async { playback.continueWatching(CONTINUE_WATCHING_LIMIT) }
                            val recent = async { catalog.browse(recentlyAddedQuery()) }
                            val top = async { catalog.browse(topRatedQuery()) }
                            HomeUiState(
                                continueWatching = resuming.await(),
                                recentlyAdded = recent.await().items,
                                topRated = top.await().items,
                            )
                        }
                    mutableState.value = LoadState.Success(loaded)
                } catch (failure: BeamException) {
                    val reason = failure.toFailure()
                    mutableState.update {
                        LoadState.Failure(
                            message = reason.message,
                            retryable = reason.retryable,
                            previous = it.previousValue(),
                        )
                    }
                }
            }
        }

        private fun LoadState<HomeUiState>.previousValue(): HomeUiState? =
            when (this) {
                is LoadState.Success -> value
                is LoadState.Loading -> previous
                is LoadState.Failure -> previous
                LoadState.Idle -> null
            }

        private fun recentlyAddedQuery() =
            BrowseQuery(
                first = ROW_LIMIT,
                after = null,
                sortBy = MediaSortField.DATE_ADDED,
                sortOrder = SortOrder.DESCENDING,
                // Null rather than a filter: the home rows mix films and series.
                mediaType = null,
                genre = null,
                year = null,
                yearFrom = null,
                yearTo = null,
                query = null,
                minRating = null,
            )

        private fun topRatedQuery() =
            recentlyAddedQuery().copy(
                sortBy = MediaSortField.RATING,
                // Unrated titles would otherwise fill a "top rated" row with titles
                // that have no rating at all, which is worse than a shorter row.
                minRating = MINIMUM_INTERESTING_RATING,
            )

        private companion object {
            const val ROW_LIMIT: UInt = 20u
            const val CONTINUE_WATCHING_LIMIT: UInt = 12u
            const val MINIMUM_INTERESTING_RATING: UInt = 1u
        }
    }
