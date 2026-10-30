package dev.beam.android.feature.admin

import dev.beam.android.core.testing.Fixtures
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import uniffi.beam_client_core.ScanJobState
import java.time.LocalDate

/** The admin screen's pure formatting: ranges, rates and scan lines. */
class AdminFormatTest {
    @Test
    fun `a range is the days ending today, today included`() {
        val today = LocalDate.of(2026, 10, 23)
        assertEquals("2026-10-17" to "2026-10-23", TelemetryRange.LAST_7_DAYS.bounds(today))
        assertEquals("2026-10-23", TelemetryRange.LAST_365_DAYS.bounds(today).second)
    }

    @Test
    fun `a failure rate with nothing attempted is zero, not a division by zero`() {
        assertEquals("0%", percent(failureRate(started = 0uL, failed = 0uL)))
        assertEquals("25%", percent(failureRate(started = 3uL, failed = 1uL)))
    }

    @Test
    fun `a scan reads as where it is and what it changed`() {
        assertEquals("Scan queued", scanSummary(Fixtures.scanJob(state = ScanJobState.QUEUED)))
        assertEquals(
            "Scanning 30 of 120",
            scanSummary(Fixtures.scanJob(state = ScanJobState.RUNNING, processed = 30uL, total = 120uL)),
        )
        assertEquals(
            "Scanning, 30 so far",
            scanSummary(Fixtures.scanJob(state = ScanJobState.RUNNING, processed = 30uL, total = null)),
        )
        assertEquals(
            "Scanned · 12 added, 3 changed, 1 missing",
            scanSummary(Fixtures.scanJob(state = ScanJobState.SUCCEEDED)),
        )
        assertEquals(
            "Scan failed: the library folder is gone",
            scanSummary(Fixtures.scanJob(state = ScanJobState.FAILED, failure = "the library folder is gone")),
        )
    }

    @Test
    fun `a scan's progress bar is determinate only once the files are counted`() {
        assertEquals(0.25f, Fixtures.scanJob(processed = 30uL, total = 120uL).scanFraction())
        assertNull(Fixtures.scanJob(total = null).scanFraction())
        assertNull(
            "an empty library is not 0% of nothing",
            Fixtures.scanJob(processed = 0uL, total = 0uL).scanFraction(),
        )
    }
}
