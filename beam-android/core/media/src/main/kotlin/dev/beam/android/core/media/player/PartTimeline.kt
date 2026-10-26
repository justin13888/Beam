package dev.beam.android.core.media.player

/**
 * Where a source's parts sit end to end, so a position in one source can be
 * carried to another.
 *
 * A multi-part movie (issue #233) is queued one item per part, and the player
 * reports positions within the part it is in. Switching to another version of
 * the same title has to land at the same moment of the film, which means
 * turning "part 2, 10 minutes in" into "1 hour 40 minutes in" and back. A part
 * whose duration is unknown ends the timeline there: nothing after it can be
 * placed, so a position past it stays in it rather than being guessed into a
 * later part.
 */
internal class PartTimeline(
    /** Each part's duration in milliseconds, in play order; null where unknown. */
    private val durationsMs: List<Long?>,
) {
    /** How far into the whole source [offsetMs] into the part at [index] is. */
    fun absoluteMs(
        index: Int,
        offsetMs: Long,
    ): Long {
        var before = 0L
        for (part in 0 until index.coerceIn(0, durationsMs.size)) {
            before += durationsMs[part] ?: return offsetMs.coerceAtLeast(0L)
        }
        return before + offsetMs.coerceAtLeast(0L)
    }

    /** The part [absoluteMs] into the whole source falls in, and how far into it. */
    fun locate(absoluteMs: Long): PartPosition {
        var remaining = absoluteMs.coerceAtLeast(0L)
        for ((index, duration) in durationsMs.withIndex()) {
            val isLast = index == durationsMs.lastIndex
            if (duration == null || isLast || remaining < duration) {
                return PartPosition(index, remaining)
            }
            remaining -= duration
        }
        return PartPosition(0, 0L)
    }
}

/** A position within one part of a source. */
internal data class PartPosition(
    /** Which part, from 0. */
    val index: Int,
    /** How far into it, in milliseconds. */
    val offsetMs: Long,
)
