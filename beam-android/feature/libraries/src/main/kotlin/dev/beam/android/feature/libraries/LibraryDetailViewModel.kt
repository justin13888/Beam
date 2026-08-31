package dev.beam.android.feature.libraries

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.model.LoadState
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.LibraryFileSummary
import uniffi.beam_client_core.LibrarySummary
import javax.inject.Inject

/** One library and the files indexed into it. */
public data class LibraryDetailUiState(
    /** The library itself. */
    val library: LibrarySummary,
    /** Its indexed files. */
    val files: List<LibraryFileSummary> = emptyList(),
)

/**
 * One library's contents.
 *
 * Shows *files*, not titles, because that is what the server can answer.
 * `GET /v1/media` takes no library parameter, so "the catalog filtered to this
 * library" is not a query that exists; `GET /v1/libraries/{id}/files` is. That
 * also makes this the screen where an operator can see what was indexed and
 * what a scan actually picked up, which the catalog view cannot show.
 */
@HiltViewModel
public class LibraryDetailViewModel
    @Inject
    constructor(
        private val catalog: CatalogRepository,
        savedStateHandle: SavedStateHandle,
    ) : ViewModel() {
        private val libraryId: String =
            requireNotNull(savedStateHandle["libraryId"]) {
                "the library detail screen needs a libraryId"
            }

        private val mutableState =
            MutableStateFlow<LoadState<LibraryDetailUiState>>(LoadState.Idle)
        public val state: StateFlow<LoadState<LibraryDetailUiState>> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Reload the library and its files. */
        public fun refresh() {
            mutableState.value = LoadState.Loading(mutableState.value.previous())
            viewModelScope.launch {
                mutableState.value =
                    try {
                        val library = catalog.library(libraryId)
                        // The file listing is allowed to fail on its own: a library
                        // whose files cannot be read is still worth naming, and the
                        // screen degrades to an empty list rather than to an error
                        // page with nothing on it.
                        val files =
                            runCatching { catalog.libraryFiles(libraryId) }
                                .getOrDefault(emptyList())
                        LoadState.Success(LibraryDetailUiState(library = library, files = files))
                    } catch (failure: BeamException) {
                        val reason = failure.toFailure()
                        LoadState.Failure(reason.message, reason.retryable, mutableState.value.previous())
                    }
            }
        }

        private fun LoadState<LibraryDetailUiState>.previous(): LibraryDetailUiState? =
            when (this) {
                is LoadState.Success -> value
                is LoadState.Loading -> previous
                is LoadState.Failure -> previous
                LoadState.Idle -> null
            }
    }
