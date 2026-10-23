package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.FilterChip
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextAlign
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.SectionHeader
import uniffi.beam_client_core.FailureRateRow
import uniffi.beam_client_core.PlaybackTelemetrySummary
import uniffi.beam_client_core.RebufferBucket
import uniffi.beam_client_core.RebufferBucketRow
import uniffi.beam_client_core.SourceSwitchRow
import uniffi.beam_client_core.SwitchTrigger
import kotlin.math.roundToInt

/**
 * The playback telemetry section of the admin area: what failed to play, how
 * often playback stalled, and how often it switched file.
 *
 * Counts only, never a title or a viewer -- the server keeps nothing finer.
 * The sums are the core's ([PlaybackTelemetrySummary]); this only lays them out.
 */
internal fun LazyListScope.playbackTelemetry(
    range: TelemetryRange,
    telemetry: PlaybackTelemetrySummary?,
    error: String?,
    onRange: (TelemetryRange) -> Unit,
) {
    item(key = "telemetry-header") { SectionHeader(title = "Playback") }
    item(key = "telemetry-range") {
        LazyRow(
            contentPadding = PaddingValues(horizontal = BeamSpacing.Medium),
            horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
        ) {
            items(TelemetryRange.entries, key = { it.name }) { option ->
                FilterChip(
                    selected = option == range,
                    onClick = { onRange(option) },
                    label = { Text(option.label) },
                )
            }
        }
    }

    when {
        error != null -> {
            item(key = "telemetry-error") {
                ListItem(
                    headlineContent = { Text("Playback telemetry is unavailable") },
                    supportingContent = { Text(error) },
                )
            }
        }

        telemetry == null -> {
            item(key = "telemetry-loading") {
                LinearProgressIndicator(
                    modifier =
                        Modifier
                            .fillMaxWidth()
                            .padding(BeamSpacing.Medium),
                )
            }
        }

        else -> {
            telemetryContent(telemetry)
        }
    }
}

private fun LazyListScope.telemetryContent(telemetry: PlaybackTelemetrySummary) {
    if (!telemetry.enabled) {
        item(key = "telemetry-off") {
            ListItem(
                headlineContent = { Text("Collection is off") },
                supportingContent = {
                    Text(
                        "Clients report nothing until BEAM_PLAYBACK_TELEMETRY_ENABLED is set. " +
                            "Counts kept while it was on are still shown.",
                    )
                },
            )
        }
    }

    item(key = "telemetry-totals") {
        Row(
            modifier =
                Modifier
                    .fillMaxWidth()
                    .padding(horizontal = BeamSpacing.Medium, vertical = BeamSpacing.Small),
            horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
        ) {
            TelemetryStat("Started", telemetry.started.toString(), Modifier.weight(1f))
            TelemetryStat(
                "Failed",
                "${telemetry.failed} · ${percent(failureRate(telemetry.started, telemetry.failed))}",
                Modifier.weight(1f),
            )
            TelemetryStat("Stalls", telemetry.rebuffers.toString(), Modifier.weight(1f))
            TelemetryStat("Switches", telemetry.sourceSwitches.toString(), Modifier.weight(1f))
        }
    }
    item(key = "telemetry-days") {
        Text(
            text = "${telemetry.from} to ${telemetry.to}, UTC · kept ${telemetry.retentionDays} days",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(horizontal = BeamSpacing.Medium),
        )
    }

    failureRates("video", "Failure rate by video codec", telemetry.byVideoCodec) { it }
    failureRates("audio", "Failure rate by audio codec", telemetry.byAudioCodec) { it }
    failureRates("client", "Failure rate by client", telemetry.byClient, ::clientLabel)

    item(key = "telemetry-rebuffers-header") { SectionHeader(title = "Stalls by length") }
    item(key = "telemetry-rebuffers") { RebufferHistogram(telemetry.rebufferHistogram) }

    item(key = "telemetry-switches-header") { SectionHeader(title = "Source switches") }
    if (telemetry.switches.isEmpty()) {
        item(key = "telemetry-switches-empty") {
            ListItem(headlineContent = { Text("No source switches") })
        }
    } else {
        items(
            telemetry.switches,
            key = { "switch-${it.clientKind}-${it.trigger}-${it.fromHeightClass}-${it.toHeightClass}" },
        ) { row -> SwitchRow(row) }
    }
}

private fun LazyListScope.failureRates(
    key: String,
    title: String,
    rows: List<FailureRateRow>,
    label: (String) -> String,
) {
    item(key = "telemetry-$key-header") { SectionHeader(title = title) }
    if (rows.isEmpty()) {
        item(key = "telemetry-$key-empty") {
            ListItem(headlineContent = { Text("No playback reported") })
        }
        return
    }
    items(rows, key = { "telemetry-$key-${it.label}" }) { row ->
        ListItem(
            headlineContent = { Text(label(row.label)) },
            supportingContent = {
                Column {
                    Text("${row.failed} of ${row.started + row.failed} failed to start")
                    LinearProgressIndicator(
                        progress = { row.failureRate.toFloat() },
                        modifier =
                            Modifier
                                .fillMaxWidth()
                                .padding(top = BeamSpacing.Tiny),
                    )
                }
            },
            trailingContent = { Text(percent(row.failureRate)) },
        )
    }
}

@Composable
private fun RebufferHistogram(rows: List<RebufferBucketRow>) {
    val largest = rows.maxOfOrNull { it.count }?.takeIf { it > 0uL } ?: 1uL
    Column(
        modifier =
            Modifier
                .fillMaxWidth()
                .padding(horizontal = BeamSpacing.Medium),
        verticalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
    ) {
        rows.forEach { row ->
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(
                    text = bucketLabel(row.bucket),
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.weight(BUCKET_LABEL_WEIGHT),
                )
                LinearProgressIndicator(
                    progress = { row.count.toFloat() / largest.toFloat() },
                    modifier = Modifier.weight(BUCKET_BAR_WEIGHT),
                )
                Text(
                    text = row.count.toString(),
                    style = MaterialTheme.typography.bodySmall,
                    textAlign = TextAlign.End,
                    modifier = Modifier.weight(BUCKET_COUNT_WEIGHT),
                )
            }
        }
    }
}

@Composable
private fun SwitchRow(row: SourceSwitchRow) {
    ListItem(
        headlineContent = {
            Text("${heightLabel(row.fromHeightClass)} to ${heightLabel(row.toHeightClass)}")
        },
        supportingContent = {
            Text(
                listOf(
                    clientLabel(row.clientKind),
                    when (row.trigger) {
                        SwitchTrigger.MANUAL -> "chosen by the viewer"
                        SwitchTrigger.AUTO -> "after sustained stalling"
                    },
                ).joinToString(" · "),
            )
        },
        trailingContent = { Text(row.count.toString()) },
    )
}

@Composable
private fun TelemetryStat(
    label: String,
    value: String,
    modifier: Modifier = Modifier,
) {
    Card(modifier = modifier) {
        Column(
            modifier =
                Modifier
                    .fillMaxWidth()
                    .padding(BeamSpacing.Compact),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text(text = value, style = MaterialTheme.typography.titleMedium, textAlign = TextAlign.Center)
            Text(
                text = label,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}

/** `failed / (started + failed)`, or zero with nothing attempted. */
internal fun failureRate(
    started: ULong,
    failed: ULong,
): Double {
    val attempts = started + failed
    return if (attempts == 0uL) 0.0 else failed.toDouble() / attempts.toDouble()
}

/** A rate from 0 to 1 as a whole percentage. */
internal fun percent(rate: Double): String = "${(rate * PERCENT).roundToInt()}%"

/** The server's client-kind names, for a person. */
internal fun clientLabel(kind: String): String =
    when (kind) {
        "android" -> "Android"
        "android_tv" -> "Android TV"
        "apple_ios" -> "iPhone and iPad"
        "apple_tvos" -> "Apple TV"
        "apple_macos" -> "Mac"
        "web_chromium" -> "Chromium browsers"
        "web_firefox" -> "Firefox"
        "web_safari" -> "Safari"
        "web_other" -> "Other browsers"
        else -> "Other"
    }

/** The server's resolution classes, for a person. */
internal fun heightLabel(heightClass: String): String =
    when (heightClass) {
        "sd" -> "SD"
        "hd" -> "720p"
        "fhd" -> "1080p"
        "uhd" -> "4K"
        else -> "Unknown"
    }

private fun bucketLabel(bucket: RebufferBucket): String =
    when (bucket) {
        RebufferBucket.UNDER1_SECS -> "Under 1 s"
        RebufferBucket.FROM1_TO3_SECS -> "1 to 3 s"
        RebufferBucket.FROM3_TO10_SECS -> "3 to 10 s"
        RebufferBucket.FROM10_TO30_SECS -> "10 to 30 s"
        RebufferBucket.AT_LEAST30_SECS -> "30 s or more"
    }

private const val PERCENT = 100
private const val BUCKET_LABEL_WEIGHT = 0.3f
private const val BUCKET_BAR_WEIGHT = 0.55f
private const val BUCKET_COUNT_WEIGHT = 0.15f
