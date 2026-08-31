package dev.beam.android.core.model

/** Where a download has got to. */
public enum class DownloadState {
    /** Accepted, not yet started. */
    Queued,

    /** Bytes are moving. */
    Downloading,

    /** Stopped by the user, resumable. */
    Paused,

    /** Waiting for a network the constraints allow. */
    WaitingForNetwork,

    /** Complete and playable offline. */
    Completed,

    /** Stopped by an error. */
    Failed,
}

/**
 * One downloaded or downloading file.
 *
 * Carries the display fields it needs rather than an id to resolve, so the
 * downloads screen renders with no network at all -- which is the entire point
 * of the feature.
 */
public data class DownloadRecord(
    /** The file being downloaded. */
    val fileId: String,
    /** The title it belongs to. */
    val mediaId: String,
    /** The episode, when the title is a series. */
    val episodeId: String? = null,
    /** The server it came from, so a removed server can take its files with it. */
    val serverId: String,
    /** Title to show. */
    val title: String,
    /** Second line, such as "S2 E4 - Return". */
    val subtitle: String? = null,
    /** Artwork, cached locally once complete. */
    val posterUrl: String? = null,
    /** Where it has got to. */
    val state: DownloadState,
    /** Bytes fetched so far. */
    val downloadedBytes: Long,
    /** Total bytes, or zero while still unknown. */
    val totalBytes: Long,
    /** Why it failed, phrased for a person. */
    val failureMessage: String? = null,
) {
    /**
     * Fraction complete, or `null` while the total is unknown.
     *
     * A determinate bar that starts at a made-up value is worse than an
     * indeterminate one, so this stays null rather than guessing.
     */
    public val progress: Float?
        get() =
            if (totalBytes > 0L) {
                (downloadedBytes.toDouble() / totalBytes.toDouble()).toFloat().coerceIn(0f, 1f)
            } else {
                null
            }

    /** Whether the file can be played with no network. */
    public val isPlayableOffline: Boolean
        get() = state == DownloadState.Completed
}
