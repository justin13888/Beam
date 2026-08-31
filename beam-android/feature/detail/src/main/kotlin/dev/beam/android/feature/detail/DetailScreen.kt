package dev.beam.android.feature.detail

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Download
import androidx.compose.material.icons.rounded.PlayArrow
import androidx.compose.material.icons.rounded.Tune
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSizes
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.Artwork
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.MetaBadgeRow
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import dev.beam.android.core.ui.Format
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.MediaSourceView

/** One title's page, wired to its view model. */
@Composable
public fun DetailRoute(
    onPlay: (fileId: String, episodeId: String?, title: String) -> Unit,
    modifier: Modifier = Modifier,
    viewModel: DetailViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    DetailScreen(
        state = state,
        onPlay = onPlay,
        onSelectSeason = viewModel::selectSeason,
        onPickSource = { viewModel.setPickingSource(true) },
        onDismissPicker = { viewModel.setPickingSource(false) },
        onDownload = viewModel::download,
        onMessageShown = viewModel::clearMessage,
        onRetry = viewModel::refresh,
        modifier = modifier,
    )
}

/** One title's page, as a function of its state. */
@Composable
internal fun DetailScreen(
    state: LoadState<DetailUiState>,
    onPlay: (fileId: String, episodeId: String?, title: String) -> Unit,
    onSelectSeason: (UInt) -> Unit,
    onPickSource: () -> Unit,
    onDismissPicker: () -> Unit,
    onDownload: (fileId: String, title: String, subtitle: String?) -> Unit,
    onMessageShown: () -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val content = state.valueOrNull
    val snackbarHost = remember { SnackbarHostState() }

    // Shown once and then cleared, so a configuration change does not replay a
    // confirmation the viewer has already read.
    LaunchedEffect(content?.message) {
        content?.message?.let { message ->
            snackbarHost.showSnackbar(message)
            onMessageShown()
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        SnackbarHost(
            hostState = snackbarHost,
            modifier = Modifier.align(Alignment.BottomCenter),
        )
    }

    when {
        content != null -> {
            DetailContent(
                state = content,
                onPlay = onPlay,
                onSelectSeason = onSelectSeason,
                onPickSource = onPickSource,
                onDismissPicker = onDismissPicker,
                onDownload = onDownload,
                modifier = modifier,
            )
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
private fun DetailContent(
    state: DetailUiState,
    onPlay: (fileId: String, episodeId: String?, title: String) -> Unit,
    onSelectSeason: (UInt) -> Unit,
    onPickSource: () -> Unit,
    onDismissPicker: () -> Unit,
    onDownload: (fileId: String, title: String, subtitle: String?) -> Unit,
    modifier: Modifier = Modifier,
) {
    val summary = state.summary

    if (state.isPickingSource) {
        SourcePicker(
            sources = state.sources,
            selection = state.selection,
            onPick = { source: MediaSourceView ->
                onDismissPicker()
                onPlay(source.fileId, null, summary.title)
            },
            onDismiss = onDismissPicker,
        )
    }

    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge),
    ) {
        item(key = "backdrop") {
            Artwork(
                url = summary.backdropUrl ?: summary.posterUrl,
                aspectRatio = BeamSizes.ThumbnailAspectRatio,
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .height(BeamSizes.BackdropHeight),
            )
        }

        item(key = "heading") {
            Column(
                modifier = Modifier.padding(BeamSpacing.Medium),
                verticalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
            ) {
                Text(text = summary.title, style = MaterialTheme.typography.headlineSmall)
                if (!summary.originalTitle.isNullOrBlank() &&
                    summary.originalTitle != summary.title
                ) {
                    Text(
                        text = summary.originalTitle!!,
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
                MetaBadgeRow(
                    labels =
                        listOfNotNull(
                            summary.year?.toString(),
                            Format.runtime(summary.runtimeMinutes).takeIf(String::isNotEmpty),
                            summary.tmdbRating?.let { "$it%" },
                        ),
                )
            }
        }

        item(key = "actions") {
            Actions(
                state = state,
                onPlay = onPlay,
                onPickSource = onPickSource,
                onDownload = onDownload,
            )
        }

        if (state.isSoftwareOnly) {
            item(key = "software-warning") {
                Text(
                    // Said plainly rather than buried in the picker: the
                    // viewer is about to start something that will stutter and
                    // drain the battery, and the alternative is one tap away.
                    text =
                        "This file plays in software on this device. Playback may " +
                            "stutter and will use more battery.",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.error,
                    modifier = Modifier.padding(horizontal = BeamSpacing.Medium),
                )
            }
        }

        summary.description?.takeIf(String::isNotBlank)?.let { description ->
            item(key = "description") {
                Text(
                    text = description,
                    style = MaterialTheme.typography.bodyMedium,
                    modifier = Modifier.padding(BeamSpacing.Medium),
                )
            }
        }

        if (summary.genres.isNotEmpty()) {
            item(key = "genres") {
                LazyRow(
                    contentPadding = PaddingValues(horizontal = BeamSpacing.Medium),
                    horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
                ) {
                    items(summary.genres, key = { it }) { genre ->
                        AssistChip(onClick = {}, label = { Text(genre) })
                    }
                }
            }
        }

        if (state.seasons.isNotEmpty()) {
            item(key = "seasons") {
                LazyRow(
                    contentPadding = PaddingValues(BeamSpacing.Medium),
                    horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
                ) {
                    items(state.seasons, key = { it.seasonNumber }) { season ->
                        FilterChip(
                            selected = season.seasonNumber == state.selectedSeason,
                            onClick = { onSelectSeason(season.seasonNumber) },
                            label = { Text("Season ${season.seasonNumber}") },
                        )
                    }
                }
            }

            item(key = "episodes-header") { SectionHeader(title = "Episodes") }

            items(state.episodes, key = { it.id }) { episode ->
                EpisodeRow(
                    episode = episode,
                    onPlay = {
                        episode.fileId?.let { fileId ->
                            onPlay(fileId, episode.id, episode.title)
                        }
                    },
                )
            }
        }
    }
}

@Composable
private fun Actions(
    state: DetailUiState,
    onPlay: (fileId: String, episodeId: String?, title: String) -> Unit,
    onPickSource: () -> Unit,
    onDownload: (fileId: String, title: String, subtitle: String?) -> Unit,
) {
    val playable = state.selection?.source
    Row(
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(horizontal = BeamSpacing.Medium),
        horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
    ) {
        Button(
            onClick = {
                playable?.let { onPlay(it.fileId, null, state.summary.title) }
            },
            enabled = playable != null,
            modifier = Modifier.weight(1f),
        ) {
            Icon(Icons.Rounded.PlayArrow, contentDescription = null)
            Text(
                text = if (playable == null) "Unavailable" else "Play",
                modifier = Modifier.padding(start = BeamSpacing.Small),
            )
        }

        // Offered for whichever file would play, so downloading and playing
        // never disagree about which one the viewer meant.
        playable?.let { source ->
            OutlinedButton(
                onClick = { onDownload(source.fileId, state.summary.title, null) },
            ) {
                Icon(Icons.Rounded.Download, contentDescription = "Download for offline")
            }
        }

        if (state.sources.size > 1) {
            OutlinedButton(onClick = onPickSource) {
                Icon(Icons.Rounded.Tune, contentDescription = "Choose a source")
            }
        }
    }
}

@Composable
private fun EpisodeRow(
    episode: EpisodeSummary,
    onPlay: () -> Unit,
) {
    // An episode with no file is listed but not playable: the series has the
    // episode, this server does not have the file, and hiding it would make a
    // gap in the numbering that looks like a bug.
    val hasFile = episode.fileId != null

    ListItem(
        headlineContent = {
            Text("${episode.episodeNumber}. ${episode.title}")
        },
        supportingContent = {
            Text(
                text =
                    episode.description?.takeIf(String::isNotBlank)
                        ?: if (hasFile) "" else "Not available on this server",
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        },
        leadingContent = {
            Box(modifier = Modifier.width(BeamSizes.ThumbnailWidth / 2)) {
                Artwork(
                    url = episode.thumbnailUrl,
                    aspectRatio = BeamSizes.ThumbnailAspectRatio,
                )
            }
        },
        trailingContent = {
            episode.durationSecs?.let { seconds ->
                Text(Format.timecode(seconds))
            }
        },
        modifier =
            Modifier
                .fillMaxWidth()
                .then(if (hasFile) Modifier.clickable(onClick = onPlay) else Modifier),
    )
}
