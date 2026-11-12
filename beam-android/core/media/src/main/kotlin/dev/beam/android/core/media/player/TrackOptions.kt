package dev.beam.android.core.media.player

import androidx.media3.common.Tracks

/** Stable identity for one track within a group. */
internal fun trackId(
    group: Tracks.Group,
    index: Int,
): String {
    val format = group.getTrackFormat(index)
    // `Format.id` is absent often enough that it cannot be the identity on its
    // own; the group and index disambiguate the rest.
    return format.id ?: "${group.type}:$index"
}

internal fun Tracks.optionsOfType(trackType: Int): List<TrackOption> =
    groups.filter { it.type == trackType }.flatMap { group ->
        (0 until group.length).map { index ->
            val format = group.getTrackFormat(index)
            TrackOption(
                id = trackId(group, index),
                label =
                    format.label
                        ?: format.language?.let(::languageName)
                        ?: "Track ${index + 1}",
                language = format.language,
                isSelected = group.isTrackSelected(index),
                isSupported = group.isTrackSupported(index),
            )
        }
    }
