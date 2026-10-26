// Media3's `Util` is @UnstableApi; see ExoBeamPlayer for why application code
// opts in at the file level.
@file:UnstableApi

package dev.beam.android.core.media.player

import androidx.media3.common.C
import androidx.media3.common.MimeTypes
import androidx.media3.common.util.UnstableApi
import androidx.media3.common.util.Util
import uniffi.beam_client_core.SubtitleTrackView
import java.util.Locale

/**
 * A subtitle file beside a media file, as ExoPlayer is handed one.
 *
 * Kept free of `Uri` and `MediaItem` so the choice -- which URL, which
 * format, which flags -- is a plain value a JVM test can assert on. The player
 * turns it into a `SubtitleConfiguration`.
 */
internal data class SidecarSubtitle(
    /** Stable identity, so a selection names this track and no other. */
    val id: String,
    /** Where to fetch it. */
    val url: String,
    /** The format ExoPlayer is told to parse it as. */
    val mimeType: String,
    /** ISO 639 language, where the file declared one. */
    val language: String?,
    /** What the subtitle picker shows. */
    val label: String?,
    /** `C.SELECTION_FLAG_*`: default and forced. */
    val selectionFlags: Int,
    /** `C.ROLE_FLAG_*`: subtitles for the deaf and hard of hearing. */
    val roleFlags: Int,
)

/**
 * How ExoPlayer should load [track], or null for one it cannot render.
 *
 * The file as stored where ExoPlayer parses its format -- SubRip, WebVTT and
 * SSA are all native to it -- since that keeps styling the WebVTT conversion
 * would drop. Anything else is loaded as its WebVTT conversion where the
 * server offers one. An image-based sidecar has neither and is left out:
 * listing a track that renders nothing would be worse than not listing it.
 */
internal fun sidecarSubtitle(track: SubtitleTrackView): SidecarSubtitle? {
    val stored = NATIVE_FORMATS[track.codec.lowercase()]
    val (url, mimeType) =
        when {
            stored != null && track.url != null -> track.url!! to stored
            track.webvttUrl != null -> track.webvttUrl!! to MimeTypes.TEXT_VTT
            else -> return null
        }
    var selection = 0
    if (track.isDefault) selection = selection or C.SELECTION_FLAG_DEFAULT
    if (track.isForced) selection = selection or C.SELECTION_FLAG_FORCED
    val roles = if (track.isHearingImpaired) C.ROLE_FLAG_DESCRIBES_MUSIC_AND_SOUND else 0
    return SidecarSubtitle(
        id = "sidecar:${track.sidecarId ?: url}",
        url = url,
        mimeType = mimeType,
        language = track.language,
        label = subtitleLabel(track),
        selectionFlags = selection,
        roleFlags = roles,
    )
}

/**
 * What the picker calls a track: its own title where it has one, then its
 * language, with "forced" and "SDH" said rather than left to a flag the picker
 * would otherwise not show. Null when there is nothing to say, so the picker
 * falls back to its own wording.
 */
internal fun subtitleLabel(track: SubtitleTrackView): String? {
    val name = track.title?.takeIf { it.isNotBlank() } ?: track.language?.let(::languageName)
    val qualifiers =
        listOfNotNull(
            "Forced".takeIf { track.isForced },
            "SDH".takeIf { track.isHearingImpaired },
        )
    return when {
        name == null && qualifiers.isEmpty() -> null
        name == null -> qualifiers.joinToString(", ")
        qualifiers.isEmpty() -> name
        else -> "$name (${qualifiers.joinToString(", ")})"
    }
}

/**
 * A language code as a person reads it: `eng` and `en` both as "English", in
 * the device's own language. A code the platform has no name for is shown as
 * it is rather than dropped, and a blank one is no name at all.
 */
internal fun languageName(code: String): String? {
    if (code.isBlank()) return null
    // ISO 639-2 codes (`eng`, `fre`) are what files and the server carry;
    // `Locale` names the two-letter form, which Media3 normalises to.
    val normalized = Util.normalizeLanguageCode(code) ?: code
    val name = Locale.forLanguageTag(normalized).getDisplayLanguage(Locale.getDefault())
    return name.takeIf { it.isNotBlank() && !it.equals(normalized, ignoreCase = true) } ?: code
}

/** The sidecar formats ExoPlayer parses as stored, by FFmpeg codec name. */
private val NATIVE_FORMATS =
    mapOf(
        "subrip" to MimeTypes.APPLICATION_SUBRIP,
        "srt" to MimeTypes.APPLICATION_SUBRIP,
        "webvtt" to MimeTypes.TEXT_VTT,
        "ass" to MimeTypes.TEXT_SSA,
        "ssa" to MimeTypes.TEXT_SSA,
    )
