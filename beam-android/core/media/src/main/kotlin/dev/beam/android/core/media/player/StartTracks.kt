package dev.beam.android.core.media.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.Tracks
import uniffi.beam_client_core.StartSubtitle
import uniffi.beam_client_core.StartTracks

/**
 * What the player does about one track type as a part starts, from the
 * tracks the server named for the viewer (issue #329).
 *
 * Kept apart from ExoPlayer itself so the matching -- which of Media3's track
 * groups the server meant -- is a plain function a JVM test can drive with
 * hand-built [Tracks].
 */
internal sealed interface StartChoice {
    /** Leave the player's own choice: nothing was named, or nothing matched. */
    data object Keep : StartChoice

    /** Start without subtitles: the server named none. */
    data object Off : StartChoice

    /** Start on [index] of [group]. */
    data class Select(
        val group: Tracks.Group,
        val index: Int,
    ) : StartChoice
}

/**
 * The audio track [start] names: the file's audio stream at its position,
 * counted the way Media3 lists them, one group per stream in container order
 * -- the order probing records them in, and so the server's `audio_tracks`.
 * A track this device cannot decode is not forced on it.
 */
internal fun Tracks.startAudio(start: StartTracks): StartChoice {
    val position = start.audioIndex?.toInt() ?: return StartChoice.Keep
    val group = groups.filter { it.type == C.TRACK_TYPE_AUDIO }.getOrNull(position) ?: return StartChoice.Keep
    return group.supportedChoice(0)
}

/**
 * The subtitle track [start] names: off when it names none; one of the file's
 * own subtitle streams by its position among them; a subtitle file beside it
 * by the identity the player loaded it under ([sidecarSubtitle]).
 */
internal fun Tracks.startSubtitle(start: StartTracks): StartChoice {
    val text = groups.filter { it.type == C.TRACK_TYPE_TEXT }
    return when (val subtitle = start.subtitle) {
        null -> {
            StartChoice.Off
        }

        is StartSubtitle.Embedded -> {
            // Closed captions carried inside the video stream surface as text
            // groups too, but are no stream of the file's own, so the server
            // never counted them.
            text
                .filter { group -> !group.isSidecar() && !group.getTrackFormat(0).isClosedCaption() }
                .getOrNull(subtitle.position.toInt())
                ?.supportedChoice(0)
                ?: StartChoice.Keep
        }

        is StartSubtitle.Sidecar -> {
            val id = "sidecar:${subtitle.sidecarId}"
            text
                .firstNotNullOfOrNull { group ->
                    (0 until group.length)
                        .firstOrNull { index -> group.getTrackFormat(index).id.namesTrack(id) }
                        ?.let { index -> group.supportedChoice(index) }
                } ?: StartChoice.Keep
        }
    }
}

private fun Tracks.Group.supportedChoice(index: Int): StartChoice =
    if (isTrackSupported(index)) StartChoice.Select(this, index) else StartChoice.Keep

private fun Tracks.Group.isSidecar(): Boolean =
    (0 until length).any {
        getTrackFormat(it).id.orEmpty().contains("sidecar:")
    }

/** A merged source may prefix the identity a track was loaded with by its child's place. */
private fun String?.namesTrack(id: String): Boolean = this == id || this?.endsWith(":$id") == true

private fun Format.isClosedCaption(): Boolean =
    listOfNotNull(sampleMimeType, codecs).any {
        it == MimeTypes.APPLICATION_CEA608 || it == MimeTypes.APPLICATION_CEA708
    }
