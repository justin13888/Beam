package dev.beam.android.feature.libraries

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.LibraryBooks
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import uniffi.beam_client_core.LibrarySummary

/** Libraries, wired to its view model. */
@Composable
public fun LibrariesRoute(
    onOpenLibrary: (String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: LibrariesViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LibrariesScreen(
        state = state,
        onOpenLibrary = onOpenLibrary,
        onRetry = viewModel::refresh,
        modifier = modifier,
    )
}

/** Libraries, as a function of its state. */
@Composable
internal fun LibrariesScreen(
    state: LoadState<List<LibrarySummary>>,
    onOpenLibrary: (String) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val libraries = state.valueOrNull

    when {
        !libraries.isNullOrEmpty() -> {
            LazyColumn(
                modifier = modifier.fillMaxSize(),
                contentPadding = PaddingValues(vertical = BeamSpacing.Small),
            ) {
                items(libraries, key = { it.id }) { library ->
                    ListItem(
                        headlineContent = { Text(library.name) },
                        supportingContent = { Text(library.summaryLine()) },
                        leadingContent = {
                            Icon(Icons.Rounded.LibraryBooks, contentDescription = null)
                        },
                        trailingContent = {
                            // A scan that started and has not finished is still
                            // running, and the file count is mid-flight -- so the
                            // indicator is the honest thing to show beside it.
                            if (library.isScanning) {
                                CircularProgressIndicator(
                                    modifier = Modifier.size(SCAN_INDICATOR),
                                    strokeWidth = 2.dp,
                                )
                            }
                        },
                        modifier =
                            Modifier
                                .fillMaxWidth()
                                .clickable { onOpenLibrary(library.id) },
                    )
                }
            }
        }

        state is LoadState.Failure -> {
            BeamErrorState(
                message = state.message,
                onRetry = onRetry.takeIf { state.retryable },
                modifier = modifier,
            )
        }

        state is LoadState.Loading || state is LoadState.Idle -> {
            BeamLoading(modifier)
        }

        else -> {
            BeamEmptyState(
                title = "No libraries yet",
                description =
                    "An administrator needs to add a library before there is " +
                        "anything to watch.",
                modifier = modifier,
            )
        }
    }
}

/** Whether a scan is in flight: started, with no finish recorded. */
internal val LibrarySummary.isScanning: Boolean
    get() = lastScanStartedAtUnix != null && lastScanFinishedAtUnix == null

/** "128 files", or an explicit statement that there are none. */
internal fun LibrarySummary.summaryLine(): String =
    when {
        isScanning -> "Scanning"
        size == 0u -> "Empty"
        size == 1u -> "1 file"
        else -> "$size files"
    }

private val SCAN_INDICATOR = 20.dp
