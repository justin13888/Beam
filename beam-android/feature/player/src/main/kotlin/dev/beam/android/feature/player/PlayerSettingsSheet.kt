package dev.beam.android.feature.player

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Check
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.media.player.PlaybackUiState
import dev.beam.android.core.media.player.TrackOption
import dev.beam.android.core.ui.Format
import uniffi.beam_client_core.MediaSourceView

/** The speeds the sheet offers, slowest first. */
internal val PLAYBACK_SPEEDS: List<Float> = listOf(0.5f, 0.75f, 1f, 1.25f, 1.5f, 2f)

/**
 * Subtitles, audio, version and speed, in one sheet over the picture.
 *
 * Every row applies at once and the sheet stays open, except a version, which
 * restarts the stream and so closes it: the viewer then sees the switch
 * happen. A track the device cannot decode is listed, greyed, rather than
 * hidden (see [TrackOption.isSupported]).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
internal fun PlayerSettingsSheet(
    playback: PlaybackUiState,
    sources: List<MediaSourceView>,
    onSubtitle: (String?) -> Unit,
    onAudio: (String) -> Unit,
    onSource: (MediaSourceView) -> Unit,
    onSpeed: (Float) -> Unit,
    onDismiss: () -> Unit,
) {
    ModalBottomSheet(onDismissRequest = onDismiss) {
        PlayerSettingsContent(
            playback = playback,
            sources = sources,
            onSubtitle = onSubtitle,
            onAudio = onAudio,
            onSource = onSource,
            onSpeed = onSpeed,
        )
    }
}

/** The sheet's rows, apart from the sheet, so they render in a plain test. */
@Composable
internal fun PlayerSettingsContent(
    playback: PlaybackUiState,
    sources: List<MediaSourceView>,
    onSubtitle: (String?) -> Unit,
    onAudio: (String) -> Unit,
    onSource: (MediaSourceView) -> Unit,
    onSpeed: (Float) -> Unit,
    modifier: Modifier = Modifier,
) {
    val playingFile = playback.request?.fileId
    LazyColumn(
        modifier = modifier.fillMaxWidth(),
        contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge),
    ) {
        item(key = "subtitles-header") { SectionHeader(title = "Subtitles") }
        item(key = "subtitles-off") {
            ChoiceRow(
                label = "Off",
                selected = playback.subtitleTracks.none { it.isSelected },
                onClick = { onSubtitle(null) },
            )
        }
        items(playback.subtitleTracks, key = { "subtitle-${it.id}" }) { track ->
            TrackRow(track = track, onClick = { onSubtitle(track.id) })
        }

        if (playback.audioTracks.size > 1) {
            item(key = "audio-header") { SectionHeader(title = "Audio") }
            items(playback.audioTracks, key = { "audio-${it.id}" }) { track ->
                TrackRow(track = track, onClick = { onAudio(track.id) })
            }
        }

        if (sources.size > 1) {
            item(key = "versions-header") { SectionHeader(title = "Version") }
            items(sources, key = { "source-${it.fileId}" }) { source ->
                ChoiceRow(
                    label = sourceLabel(source),
                    supporting = Format.fileSize(source.sizeBytes),
                    selected = playingFile != null && source.holds(playingFile),
                    onClick = { onSource(source) },
                )
            }
        }

        item(key = "speed-header") { SectionHeader(title = "Speed") }
        items(PLAYBACK_SPEEDS, key = { "speed-$it" }) { speed ->
            ChoiceRow(
                label = if (speed == 1f) "Normal" else "${speed}x",
                selected = playback.speed == speed,
                onClick = { onSpeed(speed) },
            )
        }
    }
}

@Composable
private fun TrackRow(
    track: TrackOption,
    onClick: () -> Unit,
) {
    ChoiceRow(
        label = track.label,
        supporting = if (track.isSupported) null else "Not supported on this device",
        selected = track.isSelected,
        enabled = track.isSupported,
        onClick = onClick,
    )
}

@Composable
private fun ChoiceRow(
    label: String,
    selected: Boolean,
    onClick: () -> Unit,
    supporting: String? = null,
    enabled: Boolean = true,
) {
    ListItem(
        headlineContent = {
            Text(
                text = label,
                color =
                    if (enabled) {
                        MaterialTheme.colorScheme.onSurface
                    } else {
                        MaterialTheme.colorScheme.onSurface.copy(alpha = DISABLED_ALPHA)
                    },
            )
        },
        supportingContent = supporting?.let { { Text(it) } },
        trailingContent = {
            if (selected) Icon(Icons.Rounded.Check, contentDescription = null)
        },
        modifier =
            Modifier
                .fillMaxWidth()
                .semantics { this.selected = selected }
                .clickable(enabled = enabled, role = Role.RadioButton, onClick = onClick),
    )
}

/**
 * What a version is called: its resolution, codec and HDR format, which are
 * what tell two versions of one title apart, and how many parts it plays.
 */
internal fun sourceLabel(source: MediaSourceView): String {
    val resolution = Format.resolution(source.width, source.height).takeIf { it.isNotEmpty() }
    val codec = source.videoCodec?.uppercase()
    val parts =
        source.parts.size
            .takeIf { it > 1 }
            ?.let { "$it parts" }
    return listOfNotNull(resolution, codec, source.hdrFormat, parts).joinToString(" · ").ifEmpty { "Version" }
}

/**
 * Whether [fileId] is this source's: its first file, or any part. A
 * multi-part movie playing its second part is still that version playing.
 */
internal fun MediaSourceView.holds(fileId: String): Boolean = this.fileId == fileId || parts.any { it.fileId == fileId }

private const val DISABLED_ALPHA = 0.38f
