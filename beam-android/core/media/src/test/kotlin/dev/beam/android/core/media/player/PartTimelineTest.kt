package dev.beam.android.core.media.player

import dev.beam.android.core.testing.Fixtures
import org.junit.Assert.assertEquals
import org.junit.Test

class PartTimelineTest {
    private val twoParts = PartTimeline(listOf(3_000_000L, 2_800_000L))

    @Test
    fun `a position in a later part counts every part before it`() {
        assertEquals(3_600_000L, twoParts.absoluteMs(index = 1, offsetMs = 600_000L))
        assertEquals(600_000L, twoParts.absoluteMs(index = 0, offsetMs = 600_000L))
    }

    @Test
    fun `a moment of the film is found in the part that holds it`() {
        assertEquals(PartPosition(0, 2_999_999L), twoParts.locate(2_999_999L))
        assertEquals(PartPosition(1, 0L), twoParts.locate(3_000_000L))
        assertEquals(PartPosition(1, 600_000L), twoParts.locate(3_600_000L))
    }

    @Test
    fun `locating is the inverse of counting for every part`() {
        for (index in 0..1) {
            for (offset in listOf(0L, 1L, 1_234_567L)) {
                val absolute = twoParts.absoluteMs(index, offset)
                assertEquals(PartPosition(index, offset), twoParts.locate(absolute))
            }
        }
    }

    @Test
    fun `a moment past the end stays in the last part`() {
        // The player clamps a seek past the end itself; a guessed part beyond
        // the last would not exist.
        assertEquals(PartPosition(1, 3_000_000L), twoParts.locate(6_000_000L))
    }

    @Test
    fun `a part of unknown length ends what can be placed`() {
        val unknown = PartTimeline(listOf(1_000L, null, 1_000L))
        assertEquals(PartPosition(1, 4_000L), unknown.locate(5_000L))
        // From a part whose start cannot be known, the offset is all there is.
        assertEquals(50L, unknown.absoluteMs(index = 2, offsetMs = 50L))
    }

    @Test
    fun `a whole file maps a position to itself`() {
        val whole = PartTimeline(listOf(null))
        assertEquals(PartPosition(0, 42_000L), whole.locate(42_000L))
        assertEquals(42_000L, whole.absoluteMs(0, 42_000L))
    }

    @Test
    fun `a plan's timeline is built from its parts' durations`() {
        val plan =
            Fixtures.plan(
                listOf(
                    Fixtures.part("cd1", durationSecs = 3_000.0),
                    Fixtures.part("cd2", durationSecs = 2_800.0),
                ),
            )
        assertEquals(PartPosition(1, 600_000L), plan.timeline().locate(3_600_000L))
    }
}
