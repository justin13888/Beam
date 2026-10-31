package dev.beam.android.feature.history

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.newIn
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.CursorPager
import uniffi.beam_client_core.HistoryEntry
import javax.inject.Inject

/** What has been watched, newest first. */
public data class HistoryUiState(
    /** Entries loaded so far. */
    val entries: List<HistoryEntry> = emptyList(),
    /** How many the server holds in total. */
    val total: ULong = 0uL,
    /** Whether the server holds a page after the entries loaded so far. */
    val hasNextPage: Boolean = false,
    /** Whether the first page is in flight. */
    val isLoading: Boolean = false,
    /** Whether a further page is in flight. */
    val isLoadingMore: Boolean = false,
    /** Why the last attempt failed. */
    val error: String? = null,
) {
    /** Whether there are more entries to fetch. */
    public val hasMore: Boolean
        get() = hasNextPage
}

/**
 * Watch history, paged by cursor.
 *
 * Paged through the core's [CursorPager]: playing something while the list is
 * open moves it to the top, so the same title can legitimately arrive on two
 * pages, and is listed once.
 */
@HiltViewModel
public class HistoryViewModel
    @Inject
    constructor(
        private val playback: PlaybackRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(HistoryUiState())
        public val state: StateFlow<HistoryUiState> = mutableState.asStateFlow()

        private val pager = CursorPager()
        private var inFlight: Job? = null

        init {
            addCloseable(pager)
            refresh()
        }

        /** Fetch the first page again. */
        public fun refresh() {
            // A page still arriving belongs to the list being replaced.
            inFlight?.cancel()
            pager.reset()
            pager.beginRequest()
            mutableState.update { it.copy(isLoading = true, error = null) }
            inFlight = viewModelScope.launch { fetch(replacing = true) }
        }

        /** Fetch the next page. */
        public fun loadMore() {
            val current = mutableState.value
            if (!current.hasMore || current.isLoadingMore || current.isLoading) return
            if (!pager.beginRequest()) return
            mutableState.update { it.copy(isLoadingMore = true) }
            inFlight = viewModelScope.launch { fetch(replacing = false) }
        }

        private suspend fun fetch(replacing: Boolean) {
            try {
                val page = playback.history(first = PAGE_SIZE, after = pager.cursor())
                val advance = pager.completeRequest(page.items.map(HistoryEntry::key), page.endCursor, page.hasNextPage)
                val fresh = page.items.newIn(advance, HistoryEntry::key)
                mutableState.update {
                    it.copy(
                        entries = if (replacing) fresh else it.entries + fresh,
                        total = page.total,
                        hasNextPage = advance.hasNextPage,
                        isLoading = false,
                        isLoadingMore = false,
                        error = null,
                    )
                }
            } catch (failure: BeamException) {
                pager.failRequest()
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
            const val PAGE_SIZE: UInt = 50u
        }
    }

/** A history row is one movie or one episode: the server lists each title once. */
private fun HistoryEntry.key(): String = episodeId ?: mediaId
