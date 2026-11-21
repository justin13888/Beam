package dev.beam.android.feature.admin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.model.LoadState
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.CountRange
import uniffi.beam_client_core.FileSizeRange
import uniffi.beam_client_core.LibraryTelemetryPreview
import uniffi.beam_client_core.TotalSizeRange
import javax.inject.Inject

/**
 * The anonymous library report the server would send, previewed before an
 * operator opts in (#93). Reading it sends nothing.
 */
@HiltViewModel
public class LibraryTelemetryViewModel
    @Inject
    constructor(
        private val admin: AdminRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow<LoadState<LibraryTelemetryPreview>>(LoadState.Idle)
        public val state: StateFlow<LoadState<LibraryTelemetryPreview>> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Read the preview again: the report describes the library as it is now. */
        public fun refresh() {
            val previous = (mutableState.value as? LoadState.Success)?.value
            mutableState.value = LoadState.Loading(previous)
            viewModelScope.launch {
                mutableState.value =
                    try {
                        LoadState.Success(admin.libraryTelemetryPreview())
                    } catch (failure: BeamException) {
                        val reason = failure.toFailure()
                        LoadState.Failure(reason.message, reason.retryable, previous)
                    }
            }
        }
    }

/** A count range as a person reads it. The report never holds an exact count. */
internal fun CountRange.label(): String =
    when (this) {
        CountRange.NONE -> "None"
        CountRange.FROM1_TO9 -> "1 to 9"
        CountRange.FROM10_TO99 -> "10 to 99"
        CountRange.FROM100_TO999 -> "100 to 999"
        CountRange.FROM1000_TO9999 -> "1,000 to 9,999"
        CountRange.AT_LEAST10000 -> "10,000 or more"
    }

/** A file size range as a person reads it. */
internal fun FileSizeRange.label(): String =
    when (this) {
        FileSizeRange.UNDER1_GIB -> "Under 1 GiB"
        FileSizeRange.FROM1_TO4_GIB -> "1 to 4 GiB"
        FileSizeRange.FROM4_TO10_GIB -> "4 to 10 GiB"
        FileSizeRange.FROM10_TO25_GIB -> "10 to 25 GiB"
        FileSizeRange.FROM25_TO50_GIB -> "25 to 50 GiB"
        FileSizeRange.AT_LEAST50_GIB -> "50 GiB or more"
    }

/** The library's total size range as a person reads it. */
internal fun TotalSizeRange.label(): String =
    when (this) {
        TotalSizeRange.UNDER100_GIB -> "Under 100 GiB"
        TotalSizeRange.FROM100_GIB_TO1_TIB -> "100 GiB to 1 TiB"
        TotalSizeRange.FROM1_TO10_TIB -> "1 to 10 TiB"
        TotalSizeRange.FROM10_TO50_TIB -> "10 to 50 TiB"
        TotalSizeRange.AT_LEAST50_TIB -> "50 TiB or more"
    }

/**
 * Where the report goes, for the operator deciding whether to opt in: a
 * server with no collector configured sends nothing at all.
 */
internal fun LibraryTelemetryPreview.destinationSummary(): String =
    if (!destinationConfigured) {
        "Not sending. Set BEAM_TELEMETRY_URL on the server to opt in; this is what it would send."
    } else {
        "Sending to ${destinationOrigin ?: "the configured collector"}."
    }
