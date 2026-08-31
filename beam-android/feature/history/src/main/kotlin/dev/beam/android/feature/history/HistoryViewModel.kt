package dev.beam.android.feature.history

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.HistoryEntry
import javax.inject.Inject

/** What has been watched, newest first. */
public data class HistoryUiState(
    /** Entries loaded so far. */
    val entries: List<HistoryEntry> = emptyList(),
    /** How many the server holds in total. */
    val total: ULong = 0uL,
    /** Whether the first page is in flight. */
    val isLoading: Boolean = false,
    /** Whether a further page is in flight. */
    val isLoadingMore: Boolean = false,
    /** Why the last attempt failed. */
    val error: String? = null,
) {
    /** Whether there are more entries to fetch. */
    public val hasMore: Boolean
        get() = entries.size.toULong() < total
}

/** Watch history, paged by offset. */
@HiltViewModel
public class HistoryViewModel
    @Inject
    constructor(
        private val playback: PlaybackRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(HistoryUiState())
        public val state: StateFlow<HistoryUiState> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Fetch the first page again. */
        public fun refresh() {
            mutableState.update { it.copy(isLoading = true, error = null) }
            viewModelScope.launch { fetch(offset = 0u, replacing = true) }
        }

        /** Fetch the next page. */
        public fun loadMore() {
            val current = mutableState.value
            if (!current.hasMore || current.isLoadingMore || current.isLoading) return
            mutableState.update { it.copy(isLoadingMore = true) }
            viewModelScope.launch {
                fetch(offset = current.entries.size.toUInt(), replacing = false)
            }
        }

        private suspend fun fetch(
            offset: UInt,
            replacing: Boolean,
        ) {
            try {
                val page = playback.history(limit = PAGE_SIZE, offset = offset)
                mutableState.update {
                    it.copy(
                        entries = if (replacing) page.items else it.entries + page.items,
                        total = page.total,
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
            const val PAGE_SIZE: UInt = 50u
        }
    }
