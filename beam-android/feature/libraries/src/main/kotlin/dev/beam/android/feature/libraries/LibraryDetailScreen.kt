package dev.beam.android.feature.libraries

import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.HelpOutline
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material.icons.rounded.Tv
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.style.TextOverflow
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.ui.Format
import uniffi.beam_client_core.FileContentType
import uniffi.beam_client_core.FileIndexStatus
import uniffi.beam_client_core.LibraryFileSummary

/** One library's contents, wired to its view model. */
@Composable
public fun LibraryDetailRoute(
    modifier: Modifier = Modifier,
    viewModel: LibraryDetailViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    LibraryDetailScreen(state = state, onRetry = viewModel::refresh, modifier = modifier)
}

/** One library's contents, as a function of its state. */
@Composable
internal fun LibraryDetailScreen(
    state: LoadState<LibraryDetailUiState>,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val content = state.valueOrNull

    when {
        content != null -> {
            LazyColumn(
                modifier = modifier.fillMaxSize(),
                contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge),
            ) {
                item(key = "header") {
                    ListItem(
                        headlineContent = { Text(content.library.name) },
                        supportingContent = {
                            Text(
                                content.library.description
                                    ?.takeIf(String::isNotBlank)
                                    ?: content.library.summaryLine(),
                            )
                        },
                    )
                }

                if (content.files.isEmpty()) {
                    item(key = "empty") {
                        BeamEmptyState(
                            title = "Nothing indexed yet",
                            description = "Files appear here once a scan has picked them up.",
                        )
                    }
                } else {
                    item(key = "files-header") { SectionHeader(title = "Files") }
                    items(content.files, key = { it.id }) { file -> FileRow(file) }
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

        else -> {
            BeamLoading(modifier)
        }
    }
}

@Composable
private fun FileRow(file: LibraryFileSummary) {
    ListItem(
        headlineContent = {
            Text(
                text = file.path.substringAfterLast('/'),
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        },
        supportingContent = {
            Text(
                text = file.detailLine(),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        },
        leadingContent = { Icon(file.icon(), contentDescription = null) },
    )
}

/** "MKV · 4.2 GB · 1h 56m", plus a warning when the record is stale. */
internal fun LibraryFileSummary.detailLine(): String =
    listOfNotNull(
        containerFormat?.uppercase(),
        Format.fileSize(sizeBytes).takeIf(String::isNotEmpty),
        durationSecs?.let(Format::timecode),
        // Surfaced rather than hidden: a changed or unrecognised file is the usual
        // reason a title will not play, and an operator cannot act on what the
        // screen does not say.
        when (status) {
            FileIndexStatus.CHANGED -> "Changed since the last scan"
            FileIndexStatus.UNKNOWN -> "Not yet scanned"
            FileIndexStatus.KNOWN -> null
        },
    ).joinToString(" · ")

private fun LibraryFileSummary.icon(): ImageVector =
    when (contentType) {
        FileContentType.MOVIE -> Icons.Rounded.Movie

        FileContentType.EPISODE -> Icons.Rounded.Tv

        // Unclassified means the indexer could not match it to a title, which is a
        // real state an operator needs to see rather than a rendering fallback.
        FileContentType.UNCLASSIFIED -> Icons.Rounded.HelpOutline
    }
