package dev.beam.android.core.ui

import java.util.Locale
import kotlin.math.roundToLong

/**
 * Formatting shared by every screen, so a runtime reads the same way in the
 * catalog, on a detail page, and on the lock screen.
 */
public object Format {

    /** `1h 45m`, or `45m` under an hour. Blank for an unknown duration. */
    public fun runtime(minutes: UInt?): String {
        val total = minutes?.toInt() ?: return ""
        if (total <= 0) return ""
        val hours = total / 60
        val remaining = total % 60
        return when {
            hours > 0 && remaining > 0 -> "${hours}h ${remaining}m"
            hours > 0 -> "${hours}h"
            else -> "${remaining}m"
        }
    }

    /** `1:23:45`, or `23:45` under an hour -- the scrubber and resume label. */
    public fun timecode(seconds: Double): String {
        val total = seconds.coerceAtLeast(0.0).roundToLong()
        val hours = total / 3_600
        val minutes = (total % 3_600) / 60
        val secs = total % 60
        return if (hours > 0) {
            String.format(Locale.US, "%d:%02d:%02d", hours, minutes, secs)
        } else {
            String.format(Locale.US, "%d:%02d", minutes, secs)
        }
    }

    /**
     * `18.0 GB`.
     *
     * Uses decimal units, because that is what a file size is quoted in
     * everywhere a user will compare it -- the server's own UI included.
     */
    public fun fileSize(bytes: ULong): String {
        val value = bytes.toDouble()
        val units = listOf("B", "kB", "MB", "GB", "TB")
        var index = 0
        var scaled = value
        while (scaled >= 1_000 && index < units.lastIndex) {
            scaled /= 1_000
            index++
        }
        return if (index == 0) {
            "${bytes} B"
        } else {
            String.format(Locale.US, "%.1f %s", scaled, units[index])
        }
    }

    /** `24.0 Mbps`. Blank when the bit rate is unknown. */
    public fun bitrate(bitsPerSecond: ULong?): String {
        val value = bitsPerSecond?.toDouble() ?: return ""
        if (value <= 0.0) return ""
        return String.format(Locale.US, "%.1f Mbps", value / 1_000_000)
    }

    /**
     * A resolution as people name it: `4K`, `1080p`.
     *
     * Named by height rather than by exact pixel count, because a 2.35:1 film
     * is 3840x1600 and calling that anything but 4K would confuse everyone.
     */
    public fun resolution(width: UInt?, height: UInt?): String {
        val h = height?.toInt() ?: return ""
        val w = width?.toInt() ?: 0
        return when {
            w >= 3_200 || h >= 2_000 -> "4K"
            h >= 1_400 -> "1440p"
            h >= 900 -> "1080p"
            h >= 600 -> "720p"
            h >= 400 -> "480p"
            else -> "${h}p"
        }
    }

    /** `S2 E4`, the compact episode label used on tiles. */
    public fun episodeCode(season: UInt, episode: UInt): String = "S$season E$episode"

    /** `24m left`, shown on a partially-watched tile. */
    public fun remaining(positionSecs: Double, durationSecs: Double?): String {
        val duration = durationSecs ?: return ""
        val left = (duration - positionSecs).coerceAtLeast(0.0)
        val minutes = (left / 60).roundToLong()
        return if (minutes <= 0) "Nearly finished" else "${minutes}m left"
    }
}
