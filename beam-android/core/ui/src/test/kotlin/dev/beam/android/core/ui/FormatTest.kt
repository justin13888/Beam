package dev.beam.android.core.ui

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The labels every screen shares, at the boundaries where one form gives way
 * to the next: an hour, a thousand bytes, a resolution class. Each table row
 * is `input to expected`, and a failure names the input that broke.
 */
class FormatTest {
    @Test
    fun `a runtime reads in hours and minutes, and an unknown one is blank`() {
        val cases =
            listOf(
                null to "",
                0u to "",
                1u to "1m",
                59u to "59m",
                60u to "1h",
                61u to "1h 1m",
                105u to "1h 45m",
                120u to "2h",
            )
        for ((minutes, expected) in cases) {
            assertEquals("runtime($minutes)", expected, Format.runtime(minutes))
        }
    }

    @Test
    fun `a timecode gains an hours field only from the first hour`() {
        val cases =
            listOf(
                -5.0 to "0:00",
                0.0 to "0:00",
                0.4 to "0:00",
                0.5 to "0:01",
                59.0 to "0:59",
                60.0 to "1:00",
                3_599.0 to "59:59",
                3_599.6 to "1:00:00",
                3_600.0 to "1:00:00",
                5_025.0 to "1:23:45",
                36_000.0 to "10:00:00",
            )
        for ((seconds, expected) in cases) {
            assertEquals("timecode($seconds)", expected, Format.timecode(seconds))
        }
    }

    @Test
    fun `a file size steps up a decimal unit at each thousand`() {
        val cases =
            listOf(
                0uL to "0 B",
                999uL to "999 B",
                1_000uL to "1.0 kB",
                1_500uL to "1.5 kB",
                999_999uL to "1000.0 kB",
                1_000_000uL to "1.0 MB",
                18_000_000_000uL to "18.0 GB",
                1_000_000_000_000uL to "1.0 TB",
                // Terabytes are the largest unit: nothing rolls past it.
                5_000_000_000_000_000uL to "5000.0 TB",
            )
        for ((bytes, expected) in cases) {
            assertEquals("fileSize($bytes)", expected, Format.fileSize(bytes))
        }
    }

    @Test
    fun `a bit rate reads in megabits, and an unknown or zero one is blank`() {
        val cases =
            listOf(
                null to "",
                0uL to "",
                1uL to "0.0 Mbps",
                24_000_000uL to "24.0 Mbps",
                1_250_000uL to "1.3 Mbps",
            )
        for ((bitsPerSecond, expected) in cases) {
            assertEquals("bitrate($bitsPerSecond)", expected, Format.bitrate(bitsPerSecond))
        }
    }

    @Test
    fun `a resolution is named by its height, or by a 4K width at a wide aspect`() {
        val cases =
            listOf(
                Triple(null, null, ""),
                Triple(1920u, null, ""),
                Triple(3840u, 2160u, "4K"),
                // A 2.39:1 film: 4K by width, though short of 2000 lines.
                Triple(3840u, 1600u, "4K"),
                Triple(3199u, 1999u, "1440p"),
                Triple(null, 2000u, "4K"),
                Triple(2560u, 1440u, "1440p"),
                Triple(null, 1400u, "1440p"),
                Triple(null, 1399u, "1080p"),
                Triple(1920u, 1080u, "1080p"),
                Triple(null, 900u, "1080p"),
                Triple(null, 899u, "720p"),
                Triple(null, 600u, "720p"),
                Triple(null, 599u, "480p"),
                Triple(null, 400u, "480p"),
                Triple(null, 399u, "399p"),
            )
        for ((width, height, expected) in cases) {
            assertEquals("resolution($width, $height)", expected, Format.resolution(width, height))
        }
    }

    @Test
    fun `an episode code is compact`() {
        assertEquals("S2 E4", Format.episodeCode(2u, 4u))
        assertEquals("S10 E12", Format.episodeCode(10u, 12u))
    }

    @Test
    fun `the time left rounds to minutes and is never negative`() {
        val cases =
            listOf(
                Triple(0.0, null, ""),
                Triple(1_800.0, 6_300.0, "75m left"),
                // Under half a minute left rounds to nothing left to say.
                Triple(6_271.0, 6_300.0, "Nearly finished"),
                Triple(6_269.0, 6_300.0, "1m left"),
                Triple(6_300.0, 6_300.0, "Nearly finished"),
                Triple(7_000.0, 6_300.0, "Nearly finished"),
            )
        for ((position, duration, expected) in cases) {
            assertEquals("remaining($position, $duration)", expected, Format.remaining(position, duration))
        }
    }
}
