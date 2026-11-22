package dev.beam.android.feature.admin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.newIn
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.CursorPager
import javax.inject.Inject

/** A list read a page at a time: what is loaded, and whether more follows. */
public data class PagedUiState<T>(
    /** Rows loaded so far. */
    val items: List<T> = emptyList(),
    /** How many rows the server holds in total. */
    val total: ULong = 0uL,
    /** Whether the server holds a page after the rows loaded so far. */
    val hasNextPage: Boolean = false,
    /** Whether the first page is in flight. */
    val isLoading: Boolean = false,
    /** Whether a further page is in flight. */
    val isLoadingMore: Boolean = false,
    /** Why the last attempt failed. */
    val error: String? = null,
)

/**
 * Reads a cursor-paged list into a [PagedUiState], a page at a time.
 *
 * Paged through the core's [CursorPager], so a row the server moves between
 * pages while the list is open is listed once.
 */
internal class PagedLoader<T>(
    private val scope: kotlinx.coroutines.CoroutineScope,
    private val state: MutableStateFlow<PagedUiState<T>>,
    private val key: (T) -> String,
    private val fetch: suspend (after: String?) -> Page<T>,
) {
    /** One page as the repository answered it. */
    data class Page<T>(
        val items: List<T>,
        val total: ULong,
        val endCursor: String?,
        val hasNextPage: Boolean,
    )

    val pager = CursorPager()
    private var inFlight: Job? = null

    /** Fetch the first page again, replacing what is listed. */
    fun refresh() {
        // A page still arriving belongs to the list being replaced.
        inFlight?.cancel()
        pager.reset()
        pager.beginRequest()
        state.update { it.copy(isLoading = true, error = null) }
        inFlight = scope.launch { load(replacing = true) }
    }

    /** Fetch the next page, unless none follows or one is already coming. */
    fun loadMore() {
        val current = state.value
        if (!current.hasNextPage || current.isLoadingMore || current.isLoading) return
        if (!pager.beginRequest()) return
        state.update { it.copy(isLoadingMore = true) }
        inFlight = scope.launch { load(replacing = false) }
    }

    private suspend fun load(replacing: Boolean) {
        try {
            val page = fetch(pager.cursor())
            val advance = pager.completeRequest(page.items.map(key), page.endCursor, page.hasNextPage)
            val fresh = page.items.newIn(advance, key)
            state.update {
                it.copy(
                    items = if (replacing) fresh else it.items + fresh,
                    total = page.total,
                    hasNextPage = advance.hasNextPage,
                    isLoading = false,
                    isLoadingMore = false,
                    error = null,
                )
            }
        } catch (failure: BeamException) {
            pager.failRequest()
            state.update { it.copy(isLoading = false, isLoadingMore = false, error = failure.toFailure().message) }
        }
    }
}

/** The server's operational log, newest first, a page at a time. */
@HiltViewModel
public class AdminLogsViewModel
    @Inject
    constructor(
        private val admin: AdminRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(PagedUiState<AdminLogEntry>())
        public val state: StateFlow<PagedUiState<AdminLogEntry>> = mutableState.asStateFlow()

        private val loader =
            PagedLoader(viewModelScope, mutableState, AdminLogEntry::id) { after ->
                val page = admin.logs(first = LOG_PAGE, after = after)
                PagedLoader.Page(page.items, page.total, page.endCursor, page.hasNextPage)
            }

        init {
            addCloseable(loader.pager)
            loader.refresh()
        }

        /** Read the newest lines again. */
        public fun refresh(): Unit = loader.refresh()

        /** Read the next, older page. */
        public fun loadMore(): Unit = loader.loadMore()

        private companion object {
            const val LOG_PAGE: UInt = 50u
        }
    }

/** The server's recent events, newest first. */
public data class AdminEventsUiState(
    /** The most recent events. */
    val events: List<AdminEvent> = emptyList(),
    /** Whether a read is in flight. */
    val isLoading: Boolean = false,
    /** Why the last read failed. */
    val error: String? = null,
)

/**
 * The server's event feed: a snapshot of the most recent events, read again
 * on request. The core polls rather than streams (see `admin_events`), so the
 * feed does too.
 */
@HiltViewModel
public class AdminEventsViewModel
    @Inject
    constructor(
        private val admin: AdminRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(AdminEventsUiState())
        public val state: StateFlow<AdminEventsUiState> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Read the most recent events again. */
        public fun refresh() {
            mutableState.update { it.copy(isLoading = true, error = null) }
            viewModelScope.launch {
                try {
                    val events = admin.events(EVENT_LIMIT)
                    mutableState.update { it.copy(events = events, isLoading = false) }
                } catch (failure: BeamException) {
                    // The events already on screen stay: a failed refresh says
                    // so beside them rather than blanking the feed.
                    mutableState.update { it.copy(isLoading = false, error = failure.toFailure().message) }
                }
            }
        }

        private companion object {
            const val EVENT_LIMIT: UInt = 100u
        }
    }
