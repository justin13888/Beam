package dev.beam.android.feature.admin

import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ScanJobState

/**
 * A library's latest scan, in a line: where it is and what it has done.
 *
 * Counts that are zero are left out, so a quiet rescan reads "Scanned · no
 * changes" rather than a row of zeroes.
 */
internal fun scanSummary(scan: ScanJob): String =
    when (scan.state) {
        ScanJobState.QUEUED -> {
            "Scan queued"
        }

        ScanJobState.RUNNING -> {
            scan.totalCount
                ?.let { total -> "Scanning ${scan.processedCount} of $total" }
                ?: "Scanning, ${scan.processedCount} so far"
        }

        ScanJobState.SUCCEEDED -> {
            val changes =
                listOfNotNull(
                    scan.addedCount.takeIf { it > 0uL }?.let { "$it added" },
                    scan.changedCount.takeIf { it > 0uL }?.let { "$it changed" },
                    scan.missingCount.takeIf { it > 0uL }?.let { "$it missing" },
                    scan.failedCount.takeIf { it > 0uL }?.let { "$it failed" },
                )
            "Scanned · " + changes.ifEmpty { listOf("no changes") }.joinToString(", ")
        }

        ScanJobState.FAILED -> {
            "Scan failed" + (scan.failure?.takeIf { it.isNotBlank() }?.let { ": $it" } ?: "")
        }
    }

/** How far a running scan is, once its walk has counted the files. */
internal fun ScanJob.scanFraction(): Float? {
    val total = totalCount ?: return null
    if (total == 0uL) return null
    return (processedCount.toDouble() / total.toDouble()).toFloat().coerceIn(0f, 1f)
}
