package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.EventCategory
import uniffi.beam_client_core.LogLevel
import java.time.Instant
import java.time.OffsetDateTime
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.DateTimeParseException
import java.time.format.FormatStyle

/** The operational log, newest first, a page at a time. */
@Composable
internal fun LogsScreen(
    state: PagedUiState<AdminLogEntry>,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onLoadMore: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        PageHeader(title = "Logs", onBack = onBack, onRefresh = onRefresh)
        PagedList(
            state = state,
            empty = "The server has logged nothing yet.",
            key = AdminLogEntry::id,
            onRetry = onRefresh,
            onLoadMore = onLoadMore,
        ) { entry ->
            ListItem(
                overlineContent = {
                    Text(
                        "${levelLabel(entry.level)} · ${entry.category} · ${loggedAt(entry.createdAt)}",
                    )
                },
                headlineContent = { Text(entry.message) },
                supportingContent =
                    entry.details?.let { details ->
                        { Text(details, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodySmall) }
                    },
            )
        }
    }
}

/** The server's recent events, newest first. */
@Composable
internal fun EventsScreen(
    state: AdminEventsUiState,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        PageHeader(title = "Events", onBack = onBack, onRefresh = onRefresh)
        if (state.isLoading) LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
        when {
            state.events.isEmpty() && state.error != null -> {
                BeamErrorState(message = state.error, onRetry = onRefresh)
            }

            state.events.isEmpty() && !state.isLoading -> {
                BeamEmptyState(title = "No events", description = "Scans and enrichment report here as they happen.")
            }

            else -> {
                LazyColumn(contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge)) {
                    state.error?.let { error -> item(key = "error") { ListItem(headlineContent = { Text(error) }) } }
                    items(state.events, key = { it.id }) { event -> EventRow(event) }
                }
            }
        }
    }
}

@Composable
private fun EventRow(event: AdminEvent) {
    val source =
        when (event.category) {
            EventCategory.LIBRARY_SCAN -> event.libraryName ?: "Library scan"
            EventCategory.SYSTEM -> "Server"
        }
    ListItem(
        overlineContent = { Text("${levelLabel(event.level)} · $source · ${formatUnix(event.timestampUnix)}") },
        headlineContent = { Text(event.message) },
    )
}

/**
 * A cursor-paged list: its rows, then a way to the next page while one
 * follows. A failure with nothing loaded fills the page; a failure after rows
 * are listed says so beneath them, keeping what is already read.
 */
@Composable
internal fun <T> PagedList(
    state: PagedUiState<T>,
    empty: String,
    key: (T) -> String,
    onRetry: () -> Unit,
    onLoadMore: () -> Unit,
    modifier: Modifier = Modifier,
    header: LazyListScope.() -> Unit = {},
    row: @Composable (T) -> Unit,
) {
    LazyColumn(modifier = modifier.fillMaxSize(), contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge)) {
        header()
        if (state.isLoading) item(key = "loading") { LinearProgressIndicator(modifier = Modifier.fillMaxWidth()) }
        if (state.items.isEmpty() && !state.isLoading) {
            item(key = "empty") {
                state.error?.let { BeamErrorState(message = it, onRetry = onRetry) }
                    ?: BeamEmptyState(title = "Nothing here", description = empty)
            }
        }
        items(state.items, key = key) { row(it) }
        if (state.items.isNotEmpty()) {
            state.error?.let { error -> item(key = "error") { ListItem(headlineContent = { Text(error) }) } }
            if (state.hasNextPage) {
                item(key = "more") {
                    TextButton(
                        onClick = onLoadMore,
                        enabled = !state.isLoadingMore,
                        modifier = Modifier.fillMaxWidth().padding(BeamSpacing.Small),
                    ) {
                        val shown = "${state.items.size} of ${state.total}"
                        Text(if (state.isLoadingMore) "Loading more" else "Load more ($shown)")
                    }
                }
            }
        }
    }
}

/** A severity as the log and the feed spell it. */
internal fun levelLabel(level: LogLevel): String =
    when (level) {
        LogLevel.INFO -> "Info"
        LogLevel.WARNING -> "Warning"
        LogLevel.ERROR -> "Error"
    }

private val instantFormat: DateTimeFormatter =
    DateTimeFormatter.ofLocalizedDateTime(FormatStyle.MEDIUM).withZone(ZoneId.systemDefault())

/** A Unix timestamp as this device's local date and time. */
internal fun formatUnix(
    unix: Long,
    format: DateTimeFormatter = instantFormat,
): String = format.format(Instant.ofEpochSecond(unix))

/**
 * A log line's RFC 3339 instant as this device's local date and time; text
 * that is not one is shown as the server sent it rather than hidden.
 */
internal fun loggedAt(
    rfc3339: String,
    format: DateTimeFormatter = instantFormat,
): String =
    try {
        format.format(OffsetDateTime.parse(rfc3339).toInstant())
    } catch (_: DateTimeParseException) {
        rfc3339
    }
