package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.Card
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import uniffi.beam_client_core.LibraryTelemetryPreview
import uniffi.beam_client_core.NamedCount

/**
 * The anonymous library report the server would send, as ranges and as the
 * exact bytes on the wire, and whether and where it goes.
 */
@Composable
internal fun LibraryTelemetryScreen(
    state: LoadState<LibraryTelemetryPreview>,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        PageHeader(title = "Library telemetry", onBack = onBack, onRefresh = onRefresh)
        val preview = state.valueOrNull
        when {
            preview != null -> {
                if (state is LoadState.Loading) LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                LazyColumn(contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge)) {
                    if (state is LoadState.Failure) {
                        item(key = "error") { ListItem(headlineContent = { Text(state.message) }) }
                    }
                    previewSections(preview)
                }
            }

            state is LoadState.Failure -> {
                BeamErrorState(message = state.message, onRetry = onRefresh.takeIf { state.retryable })
            }

            else -> {
                BeamLoading()
            }
        }
    }
}

private fun LazyListScope.previewSections(preview: LibraryTelemetryPreview) {
    val report = preview.report
    item(key = "destination") {
        ListItem(
            headlineContent = { Text(preview.destinationSummary()) },
            supportingContent = {
                Text(
                    listOfNotNull(
                        "Every number is a range; no title, path or account is included.",
                        preview.lastSentAtUnix?.let { "Last sent ${formatUnix(it)}." },
                        preview.nextSendAtUnix?.let { "Next due ${formatUnix(it)}." },
                    ).joinToString(" "),
                )
            },
        )
    }

    item(key = "report-header") { SectionHeader(title = "Report for ${report.generatedOn}") }
    rangeRow("Libraries", report.libraries.label())
    rangeRow("Movies", report.movies.label())
    rangeRow("Shows", report.shows.label())
    rangeRow("Seasons", report.seasons.label())
    rangeRow("Episodes", report.episodes.label())
    rangeRow("Movie files", report.movieFiles.label())
    rangeRow("Episode files", report.episodeFiles.label())
    rangeRow("Unclassified files", report.unclassifiedFiles.label())
    rangeRow("Total size", report.totalSize.label())
    namedCounts("Containers", report.containers)
    namedCounts("Video codecs", report.videoCodecs)
    namedCounts("Audio codecs", report.audioCodecs)
    namedCounts("Subtitle codecs", report.subtitleCodecs)
    if (report.fileSizes.isNotEmpty()) {
        item(key = "sizes-header") { SectionHeader(title = "File sizes") }
        report.fileSizes.forEach { size ->
            rangeRow(size.range.label(), size.count.label(), key = "size-${size.range.name}")
        }
    }

    item(key = "payload-header") { SectionHeader(title = "Exactly what is sent (${preview.contentType})") }
    item(key = "payload") {
        Card(modifier = Modifier.fillMaxWidth().padding(horizontal = BeamSpacing.Medium)) {
            SelectionContainer {
                Text(
                    text = preview.payload,
                    fontFamily = FontFamily.Monospace,
                    style = MaterialTheme.typography.bodySmall,
                    modifier = Modifier.padding(BeamSpacing.Compact),
                )
            }
        }
    }
}

private fun LazyListScope.rangeRow(
    label: String,
    range: String,
    key: String = "range-$label",
) {
    item(key = key) { ListItem(headlineContent = { Text(label) }, trailingContent = { Text(range) }) }
}

private fun LazyListScope.namedCounts(
    title: String,
    counts: List<NamedCount>,
) {
    if (counts.isEmpty()) return
    item(key = "$title-header") { SectionHeader(title = title) }
    counts.forEach { count -> rangeRow(count.name, count.count.label(), key = "$title-${count.name}") }
}
