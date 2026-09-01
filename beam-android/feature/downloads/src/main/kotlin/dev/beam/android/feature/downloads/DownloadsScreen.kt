package dev.beam.android.feature.downloads

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Delete
import androidx.compose.material.icons.rounded.Pause
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.ui.Format
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState
import uniffi.beam_client_core.downloadFraction
import uniffi.beam_client_core.downloadIsPlayableOffline

/** Downloads, wired to its view model. */
@Composable
public fun DownloadsRoute(
    onPlay: (DownloadRecord) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: DownloadsViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    DownloadsScreen(
        state = state,
        onPlay = onPlay,
        onPause = viewModel::pause,
        onResume = viewModel::resume,
        onRemove = viewModel::remove,
        modifier = modifier,
    )
}

/** Downloads, as a function of its state. */
@Composable
internal fun DownloadsScreen(
    state: DownloadsUiState,
    onPlay: (DownloadRecord) -> Unit,
    onPause: (String) -> Unit,
    onResume: (String) -> Unit,
    onRemove: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (state.isEmpty) {
        BeamEmptyState(
            title = "No downloads",
            description =
                "Download a film or an episode and it will play here with " +
                    "no connection at all.",
            modifier = modifier,
        )
        return
    }

    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(vertical = BeamSpacing.Small),
    ) {
        section("In progress", state.inProgress, onPlay, onPause, onResume, onRemove)
        section("Failed", state.failed, onPlay, onPause, onResume, onRemove)
        section("Downloaded", state.completed, onPlay, onPause, onResume, onRemove)
    }
}

private fun LazyListScope.section(
    title: String,
    records: List<DownloadRecord>,
    onPlay: (DownloadRecord) -> Unit,
    onPause: (String) -> Unit,
    onResume: (String) -> Unit,
    onRemove: (String) -> Unit,
) {
    if (records.isEmpty()) return
    item(key = "$title-header") { SectionHeader(title = title) }
    items(records, key = { it.fileId }) { record ->
        DownloadRow(
            record = record,
            onPlay = { onPlay(record) },
            onPause = { onPause(record.fileId) },
            onResume = { onResume(record.fileId) },
            onRemove = { onRemove(record.fileId) },
        )
    }
}

@Composable
private fun DownloadRow(
    record: DownloadRecord,
    onPlay: () -> Unit,
    onPause: () -> Unit,
    onResume: () -> Unit,
    onRemove: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(record.title) },
        supportingContent = {
            Text(record.statusLine())
            val progress = downloadFraction(record)?.toFloat()
            if (record.state == DownloadState.Downloading) {
                if (progress != null) {
                    LinearProgressIndicator(
                        progress = { progress },
                        modifier =
                            Modifier
                                .fillMaxWidth()
                                .padding(top = BeamSpacing.Tiny),
                    )
                } else {
                    // The total is not known yet, so an indeterminate bar is
                    // the only honest one to draw.
                    LinearProgressIndicator(
                        modifier =
                            Modifier
                                .fillMaxWidth()
                                .padding(top = BeamSpacing.Tiny),
                    )
                }
            }
        },
        trailingContent = {
            when (record.state) {
                is DownloadState.Downloading, is DownloadState.Queued -> {
                    IconButton(onClick = onPause) {
                        Icon(Icons.Rounded.Pause, contentDescription = "Pause")
                    }
                }

                is DownloadState.Paused,
                is DownloadState.Failed,
                is DownloadState.WaitingForNetwork,
                -> {
                    IconButton(onClick = onResume) {
                        Icon(Icons.Rounded.PlayArrow, contentDescription = "Resume")
                    }
                }

                is DownloadState.Completed -> {
                    IconButton(onClick = onRemove) {
                        Icon(Icons.Rounded.Delete, contentDescription = "Delete")
                    }
                }
            }
        },
        modifier =
            Modifier
                .fillMaxWidth()
                .then(
                    if (downloadIsPlayableOffline(record)) {
                        Modifier.clickable(onClick = onPlay)
                    } else {
                        Modifier
                    },
                ),
    )
}

/** "1.2 GB of 4.0 GB", or why it stopped. */
internal fun DownloadRecord.statusLine(): String =
    when (val current = state) {
        is DownloadState.Completed -> {
            Format.fileSize(totalBytes ?: downloadedBytes)
        }

        is DownloadState.Failed -> {
            current.message
        }

        is DownloadState.WaitingForNetwork -> {
            "Waiting for Wi-Fi"
        }

        is DownloadState.Paused -> {
            "Paused · ${Format.fileSize(downloadedBytes)}"
        }

        is DownloadState.Queued -> {
            "Waiting to start"
        }

        is DownloadState.Downloading -> {
            val total = totalBytes
            if (total != null && total > 0uL) {
                "${Format.fileSize(downloadedBytes)} of ${Format.fileSize(total)}"
            } else {
                Format.fileSize(downloadedBytes)
            }
        }
    }
