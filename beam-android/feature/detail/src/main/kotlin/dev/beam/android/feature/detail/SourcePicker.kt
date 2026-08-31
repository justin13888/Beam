package dev.beam.android.feature.detail

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.navigationBarsPadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Block
import androidx.compose.material.icons.rounded.Bolt
import androidx.compose.material.icons.rounded.Memory
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.ui.Format
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.Playability
import uniffi.beam_client_core.RejectedSource
import uniffi.beam_client_core.SourceSelection

/**
 * The files behind a title, and whether this device can play each one.
 *
 * Unplayable sources are listed, disabled, with the reason -- never hidden.
 * Beam does not transcode ([ADR-0004]), so "this device cannot play this file"
 * is a permanent property of the pairing and something the viewer may need to
 * act on: transcode it themselves, or watch on another device. A source that
 * silently vanished would leave them unable to tell a missing file from an
 * unplayable one.
 */
@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
internal fun SourcePicker(
    sources: List<MediaSourceView>,
    selection: SourceSelection?,
    onPick: (MediaSourceView) -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val rejections: Map<String, RejectedSource> =
        selection?.rejected.orEmpty().associateBy { it.fileId }

    ModalBottomSheet(onDismissRequest = onDismiss, modifier = modifier) {
        Column(modifier = Modifier.navigationBarsPadding()) {
            Text(
                text = "Choose a source",
                style = MaterialTheme.typography.titleLarge,
                modifier =
                    Modifier.padding(
                        start = BeamSpacing.Medium,
                        end = BeamSpacing.Medium,
                        bottom = BeamSpacing.Small,
                    ),
            )
            selection?.reason?.takeIf(String::isNotBlank)?.let { reason ->
                Text(
                    text = reason,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier =
                        Modifier.padding(
                            start = BeamSpacing.Medium,
                            end = BeamSpacing.Medium,
                            bottom = BeamSpacing.Medium,
                        ),
                )
            }

            LazyColumn {
                items(sources, key = { it.fileId }) { source ->
                    val rejection = rejections[source.fileId]
                    val isSelected = selection?.source?.fileId == source.fileId
                    val playable = rejection == null

                    ListItem(
                        headlineContent = { Text(source.describe()) },
                        supportingContent = {
                            Text(
                                text =
                                    rejection?.detail
                                        ?: source.detailLine()
                                        ?: playabilityNote(selection, isSelected),
                            )
                        },
                        leadingContent = {
                            Icon(
                                imageVector = source.icon(selection, rejection),
                                contentDescription = null,
                            )
                        },
                        colors =
                            if (playable) {
                                ListItemDefaults.colors()
                            } else {
                                // Greyed rather than removed: the file exists, and
                                // the viewer is entitled to know it exists and why
                                // it will not play here.
                                ListItemDefaults.colors(
                                    headlineColor = MaterialTheme.colorScheme.onSurfaceVariant,
                                    supportingColor = MaterialTheme.colorScheme.onSurfaceVariant,
                                    leadingIconColor = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            },
                        modifier =
                            Modifier
                                .fillMaxWidth()
                                .then(
                                    if (playable) {
                                        Modifier.clickable { onPick(source) }
                                    } else {
                                        Modifier
                                    },
                                ),
                    )
                }
            }
        }
    }
}

/** "1080p · HEVC · MKV · 4.2 GB". */
internal fun MediaSourceView.describe(): String =
    listOfNotNull(
        Format.resolution(width, height).takeIf(String::isNotEmpty),
        videoCodec?.uppercase(),
        container?.uppercase(),
        Format.fileSize(sizeBytes).takeIf(String::isNotEmpty),
    ).joinToString(" · ")

/** The second line: bitrate, HDR, and how many audio tracks. */
internal fun MediaSourceView.detailLine(): String? =
    listOfNotNull(
        Format.bitrate(bitRate).takeIf(String::isNotEmpty),
        hdrFormat?.uppercase(),
        audioTracks.size.takeIf { it > 1 }?.let { "$it audio tracks" },
    ).joinToString(" · ").takeIf(String::isNotBlank)

private fun playabilityNote(
    selection: SourceSelection?,
    isSelected: Boolean,
): String =
    when {
        !isSelected -> {
            "Playable"
        }

        selection?.playability is Playability.Software -> {
            "Plays in software on this device, which uses more battery"
        }

        else -> {
            "Plays with hardware decoding"
        }
    }

private fun MediaSourceView.icon(
    selection: SourceSelection?,
    rejection: RejectedSource?,
): ImageVector =
    when {
        rejection != null -> Icons.Rounded.Block

        selection?.source?.fileId == fileId &&
            selection.playability is Playability.Software -> Icons.Rounded.Memory

        else -> Icons.Rounded.Bolt
    }
