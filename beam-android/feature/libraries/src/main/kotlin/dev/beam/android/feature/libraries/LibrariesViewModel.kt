package dev.beam.android.feature.libraries

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
import uniffi.beam_client_core.LibrarySummary
import javax.inject.Inject

/** Every library on the server. */
@HiltViewModel
public class LibrariesViewModel
    @Inject
    constructor(
        private val catalog: CatalogRepository,
    ) : ViewModel() {
        private val mutableState =
            MutableStateFlow<LoadState<List<LibrarySummary>>>(LoadState.Idle)
        public val state: StateFlow<LoadState<List<LibrarySummary>>> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Reload the list. */
        public fun refresh() {
            mutableState.value = LoadState.Loading(mutableState.value.previous())
            viewModelScope.launch {
                mutableState.value =
                    try {
                        LoadState.Success(catalog.libraries())
                    } catch (failure: BeamException) {
                        val reason = failure.toFailure()
                        LoadState.Failure(reason.message, reason.retryable, mutableState.value.previous())
                    }
            }
        }

        private fun LoadState<List<LibrarySummary>>.previous(): List<LibrarySummary>? =
            when (this) {
                is LoadState.Success -> value
                is LoadState.Loading -> previous
                is LoadState.Failure -> previous
                LoadState.Idle -> null
            }
    }
