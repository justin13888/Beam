package dev.beam.android.feature.admin

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import uniffi.beam_client_core.LibrarySummary
import kotlin.time.Duration.Companion.seconds

/** The admin dashboard, as a function of its state. */
@Composable
internal fun AdminScreen(
    state: LoadState<AdminUiState>,
    onScan: (String) -> Unit,
    onDeleteLibrary: (String) -> Unit,
    onSetUserDisabled: (String, Boolean) -> Unit,
    onRetry: () -> Unit,
    modifier: Modifier = Modifier,
    onTelemetryRange: (TelemetryRange) -> Unit = {},
    onCreateLibrary: (name: String, rootPath: String) -> Unit = { _, _ -> },
    onMessageShown: () -> Unit = {},
    onOpen: (AdminPage) -> Unit = {},
) {
    val content = state.valueOrNull
    val snackbarHost = remember { SnackbarHostState() }

    // Shown once and then cleared, so a configuration change does not replay
    // a confirmation the operator has already read.
    LaunchedEffect(content?.message) {
        content?.message?.let { message ->
            snackbarHost.showSnackbar(message)
            onMessageShown()
        }
    }

    Box(modifier = Modifier.fillMaxSize()) {
        SnackbarHost(
            hostState = snackbarHost,
            modifier = Modifier.align(Alignment.BottomCenter),
        )
    }

    when {
        content != null -> {
            AdminContent(
                state = content,
                onScan = onScan,
                onDeleteLibrary = onDeleteLibrary,
                onSetUserDisabled = onSetUserDisabled,
                onTelemetryRange = onTelemetryRange,
                onCreateLibrary = onCreateLibrary,
                onRefresh = onRetry,
                onOpen = onOpen,
                modifier = modifier,
            )
        }

        state is LoadState.Failure -> {
            BeamErrorState(
                message = state.message,
                onRetry = onRetry.takeIf { state.retryable },
                modifier = modifier,
            )
        }

        else -> {
            BeamLoading(modifier)
        }
    }
}

@Composable
private fun AdminContent(
    state: AdminUiState,
    onScan: (String) -> Unit,
    onDeleteLibrary: (String) -> Unit,
    onSetUserDisabled: (String, Boolean) -> Unit,
    onTelemetryRange: (TelemetryRange) -> Unit,
    onCreateLibrary: (name: String, rootPath: String) -> Unit,
    onRefresh: () -> Unit,
    onOpen: (AdminPage) -> Unit,
    modifier: Modifier = Modifier,
) {
    val status = state.status
    var isAddingLibrary by rememberSaveable { mutableStateOf(false) }
    var deleting by remember { mutableStateOf<LibrarySummary?>(null) }

    if (isAddingLibrary) {
        AddLibraryDialog(
            onAdd = { name, path ->
                isAddingLibrary = false
                onCreateLibrary(name, path)
            },
            onDismiss = { isAddingLibrary = false },
        )
    }

    // Deleting a library forgets everything indexed into it -- its titles,
    // watch history and matches -- so it is never one tap. The media itself
    // is never touched: Beam only reads the library's folder.
    deleting?.let { library ->
        AlertDialog(
            onDismissRequest = { deleting = null },
            title = { Text("Delete ${library.name}?") },
            text = {
                Text(
                    "Beam forgets everything it indexed from this library, and the " +
                        "watch history that goes with it. The files themselves are not touched.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        deleting = null
                        onDeleteLibrary(library.id)
                    },
                ) { Text("Delete") }
            },
            dismissButton = { TextButton(onClick = { deleting = null }) { Text("Cancel") } },
        )
    }

    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge),
    ) {
        item(key = "server-header") { SectionHeader(title = "Server") }
        item(key = "counts") {
            Row(
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .padding(horizontal = BeamSpacing.Medium),
                horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
            ) {
                StatCard("Libraries", status.counts.libraries.toString(), Modifier.weight(1f))
                StatCard("Files", status.counts.files.toString(), Modifier.weight(1f))
                StatCard("Users", status.counts.users.toString(), Modifier.weight(1f))
            }
        }
        item(key = "version") {
            ListItem(
                headlineContent = { Text("Version ${status.version}") },
                supportingContent = { Text("Up ${status.uptimeSecs.toLong().seconds}") },
            )
        }

        item(key = "enrichment-header") { SectionHeader(title = "Metadata") }
        item(key = "enrichment") {
            Row(
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .padding(horizontal = BeamSpacing.Medium),
                horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
            ) {
                StatCard(
                    "Enriched",
                    status.enrichment.enriched.toString(),
                    Modifier.weight(1f),
                )
                StatCard("Pending", status.enrichment.pending.toString(), Modifier.weight(1f))
                // Failed and unmatched are shown separately because they need
                // different responses: a failure may resolve on a retry, an
                // unmatched file needs someone to rename it.
                StatCard("Failed", status.enrichment.failed.toString(), Modifier.weight(1f))
                StatCard(
                    "Unmatched",
                    status.enrichment.unmatched.toString(),
                    Modifier.weight(1f),
                )
            }
        }
        item(key = "open-metadata") {
            OpenPageRow(
                "Fix matches and lock fields",
                "${status.enrichment.unmatched} unmatched, ${status.enrichment.failed} failed",
            ) { onOpen(AdminPage.Metadata) }
        }

        item(key = "operations-header") { SectionHeader(title = "Operations") }
        item(key = "open-logs") { OpenPageRow("Logs", "The server's operational log") { onOpen(AdminPage.Logs) } }
        item(
            key = "open-events",
        ) { OpenPageRow("Events", "Scans, enrichment and system events") { onOpen(AdminPage.Events) } }
        item(key = "open-library-telemetry") {
            OpenPageRow("Library telemetry", "The anonymous report this server would send") {
                onOpen(AdminPage.LibraryTelemetry)
            }
        }

        item(key = "libraries-header") {
            SectionHeader(title = "Libraries", actionLabel = "Refresh", onAction = onRefresh)
        }
        items(state.libraries, key = { it.id }) { library ->
            LibraryRow(
                library = library,
                scan = state.scans[library.id],
                isStartingScan = state.scanningLibraryId == library.id,
                onScan = { onScan(library.id) },
                onDelete = { deleting = library },
            )
        }
        item(key = "add-library") {
            ListItem(
                headlineContent = { Text("Add a library") },
                supportingContent = { Text("A folder on the server, read but never written to.") },
                leadingContent = { Icon(Icons.Rounded.Add, contentDescription = null) },
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .clickable { isAddingLibrary = true },
            )
        }

        if (state.users.isNotEmpty()) {
            item(key = "users-header") { SectionHeader(title = "Users") }
            items(state.users, key = { it.id }) { user ->
                UserRow(user = user, onSetDisabled = onSetUserDisabled)
            }
        }

        if (status.recentScans.isNotEmpty()) {
            item(key = "scans-header") { SectionHeader(title = "Recent activity") }
            items(status.recentScans, key = { "${it.timestampUnix}-${it.message}" }) { scan ->
                ListItem(
                    headlineContent = { Text(scan.message) },
                    supportingContent = { Text(scan.level.name.lowercase()) },
                )
            }
        }

        playbackTelemetry(
            range = state.telemetryRange,
            telemetry = state.telemetry,
            error = state.telemetryError,
            onRange = onTelemetryRange,
        )
    }
}
