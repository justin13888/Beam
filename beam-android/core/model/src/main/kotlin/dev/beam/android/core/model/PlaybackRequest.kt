package dev.beam.android.core.model

/**
 * Everything needed to start playback, assembled before the player is built.
 *
 * The player module takes this rather than reaching back into a repository,
 * which keeps it testable without a network and makes "what exactly are we
 * about to play" a single inspectable value.
 */
public data class PlaybackRequest(
    /** The title being played. */
    val mediaId: String,
    /** The episode, when the title is a series. */
    val episodeId: String? = null,
    /** The file to stream or read from disk. */
    val fileId: String,
    /** Title shown in the player and on the lock screen. */
    val title: String,
    /** Second line, such as "S2 E4 - Return". */
    val subtitle: String? = null,
    /** Artwork for the media session. */
    val artworkUrl: String? = null,
    /** Where to resume from. */
    val startPositionSecs: Double = 0.0,
    /** Total duration where known, for the initial scrubber. */
    val durationSecs: Double? = null,
)
