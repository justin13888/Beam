package dev.beam.android.feature.explore

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.FlowPreview
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.debounce
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.BrowseQuery
import uniffi.beam_client_core.MediaSortField
import uniffi.beam_client_core.MediaSummary
import uniffi.beam_client_core.MediaTypeFilter
import uniffi.beam_client_core.SortOrder
import javax.inject.Inject

/** The catalog, with whatever filters are applied. */
public data class ExploreUiState(
    /** Titles loaded so far, across every page fetched. */
    val items: List<MediaSummary> = emptyList(),
    /** Genres offered as filter chips. */
    val genres: List<String> = emptyList(),
    /** What the viewer has typed into the search field. */
    val query: String = "",
    /** The genre filter, or null for all genres. */
    val genre: String? = null,
    /** Films, series, or both. */
    val mediaType: MediaTypeFilter? = null,
    /** What to order by. */
    val sortBy: MediaSortField = MediaSortField.TITLE,
    /** Which direction to order in. */
    val sortOrder: SortOrder = SortOrder.ASCENDING,
    /** Whether the first page is in flight. */
    val isLoading: Boolean = false,
    /** Whether a further page is in flight. */
    val isLoadingMore: Boolean = false,
    /** Whether the server says there is more to fetch. */
    val hasMore: Boolean = false,
    /** Why the last attempt failed. */
    val error: String? = null,
) {
    /** Whether any filter narrows the catalog. */
    public val hasFilters: Boolean
        get() = query.isNotBlank() || genre != null || mediaType != null
}

/**
 * Browsing and searching the catalog.
 *
 * Search is debounced, which matters more here than in most places: every
 * keystroke would otherwise be a query against the whole catalog, and the
 * results for a half-typed word are never what the viewer wanted. The cursor
 * is reset whenever a filter changes, because a cursor is only meaningful
 * within the query that produced it -- reusing one across a filter change
 * returns a page from the middle of a different result set.
 */
@OptIn(FlowPreview::class)
@HiltViewModel
public class ExploreViewModel
    @Inject
    constructor(
        private val catalog: CatalogRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(ExploreUiState())
        public val state: StateFlow<ExploreUiState> = mutableState.asStateFlow()

        private var cursor: String? = null
        private var inFlight: Job? = null

        init {
            viewModelScope.launch {
                runCatching { catalog.genres() }
                    .onSuccess { genres -> mutableState.update { it.copy(genres = genres) } }
            }
            viewModelScope.launch {
                mutableState
                    .map { it.query }
                    .distinctUntilChanged()
                    // Dropped, because the first emission is the initial empty
                    // query, which the explicit reload below already covers.
                    // Without this the screen fetches its first page twice.
                    .drop(1)
                    .debounce(SEARCH_DEBOUNCE_MS)
                    .collect { reload() }
            }
            reload()
        }

        /** The viewer typed in the search field. */
        public fun onQueryChange(value: String) {
            mutableState.update { it.copy(query = value) }
        }

        /** Restrict to one genre, or clear the restriction. */
        public fun onGenreChange(genre: String?) {
            mutableState.update { it.copy(genre = genre) }
            reload()
        }

        /** Restrict to films, series, or neither. */
        public fun onMediaTypeChange(mediaType: MediaTypeFilter?) {
            mutableState.update { it.copy(mediaType = mediaType) }
            reload()
        }

        /** Change the ordering. */
        public fun onSortChange(
            sortBy: MediaSortField,
            sortOrder: SortOrder,
        ) {
            mutableState.update { it.copy(sortBy = sortBy, sortOrder = sortOrder) }
            reload()
        }

        /** Clear every filter. */
        public fun clearFilters() {
            mutableState.update {
                it.copy(query = "", genre = null, mediaType = null)
            }
            reload()
        }

        /** Fetch the first page again, discarding what is loaded. */
        public fun reload() {
            inFlight?.cancel()
            cursor = null
            mutableState.update { it.copy(isLoading = true, error = null) }
            inFlight =
                viewModelScope.launch {
                    fetch(replacing = true)
                }
        }

        /** Fetch the next page, if there is one. */
        public fun loadMore() {
            val current = mutableState.value
            if (!current.hasMore || current.isLoadingMore || current.isLoading) return

            mutableState.update { it.copy(isLoadingMore = true) }
            inFlight =
                viewModelScope.launch {
                    fetch(replacing = false)
                }
        }

        private suspend fun fetch(replacing: Boolean) {
            val current = mutableState.value
            try {
                val page =
                    catalog.browse(
                        BrowseQuery(
                            first = PAGE_SIZE,
                            after = cursor,
                            sortBy = current.sortBy,
                            sortOrder = current.sortOrder,
                            mediaType = current.mediaType,
                            genre = current.genre,
                            year = null,
                            yearFrom = null,
                            yearTo = null,
                            query = current.query.trim().takeIf(String::isNotEmpty),
                            minRating = null,
                        ),
                    )
                cursor = page.endCursor
                mutableState.update {
                    it.copy(
                        items = if (replacing) page.items else it.items + page.items,
                        hasMore = page.hasNextPage,
                        isLoading = false,
                        isLoadingMore = false,
                        error = null,
                    )
                }
            } catch (failure: BeamException) {
                mutableState.update {
                    it.copy(
                        isLoading = false,
                        isLoadingMore = false,
                        error = failure.toFailure().message,
                    )
                }
            }
        }

        private companion object {
            /**
             * Matches `beam-web`'s `useDebouncedValue`, so the two clients feel
             * the same rather than one seeming laggier than the other.
             */
            const val SEARCH_DEBOUNCE_MS = 300L
            const val PAGE_SIZE: UInt = 40u
        }
    }
