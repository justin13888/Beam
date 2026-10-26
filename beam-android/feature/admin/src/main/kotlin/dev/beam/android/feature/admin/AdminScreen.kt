package dev.beam.android.feature.admin

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Add
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
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
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.model.valueOrNull
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ScanJobState
import kotlin.time.Duration.Companion.seconds

/** The admin area, wired to its view model. */
@Composable
public fun AdminRoute(
    modifier: Modifier = Modifier,
    viewModel: AdminViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    AdminScreen(
        state = state,
        onScan = viewModel::scan,
        onDeleteLibrary = viewModel::deleteLibrary,
        onSetUserDisabled = viewModel::setUserDisabled,
        onTelemetryRange = viewModel::setTelemetryRange,
        onRetry = viewModel::refresh,
        modifier = modifier,
        onCreateLibrary = viewModel::createLibrary,
        onMessageShown = viewModel::clearMessage,
    )
}

/** The admin area, as a function of its state. */
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

@Composable
private fun StatCard(
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
            horizontalAlignment = androidx.compose.ui.Alignment.CenterHorizontally,
        ) {
            Text(text = value, style = MaterialTheme.typography.headlineSmall)
            Text(
                text = label,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
        }
    }
}

@Composable
private fun LibraryRow(
    library: LibrarySummary,
    scan: ScanJob?,
    isStartingScan: Boolean,
    onScan: () -> Unit,
    onDelete: () -> Unit,
) {
    val underWay = scan?.state?.isUnderWay() == true
    ListItem(
        headlineContent = { Text(library.name) },
        supportingContent = {
            Column(verticalArrangement = Arrangement.spacedBy(BeamSpacing.Tiny)) {
                Text(listOfNotNull("${library.size} files", scan?.let(::scanSummary)).joinToString(" · "))
                if (underWay) {
                    // Determinate once the walk has counted the files, until
                    // then an honest "working on it".
                    val fraction = scan?.scanFraction()
                    if (fraction != null) {
                        LinearProgressIndicator(progress = { fraction }, modifier = Modifier.fillMaxWidth())
                    } else {
                        LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                    }
                }
            }
        },
        trailingContent = {
            if (isStartingScan) {
                CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
            } else {
                Row {
                    TextButton(onClick = onScan, enabled = !underWay) { Text("Scan") }
                    TextButton(onClick = onDelete) { Text("Delete") }
                }
            }
        },
    )
}

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

@Composable
private fun AddLibraryDialog(
    onAdd: (name: String, rootPath: String) -> Unit,
    onDismiss: () -> Unit,
) {
    var name by rememberSaveable { mutableStateOf("") }
    var path by rememberSaveable { mutableStateOf("") }
    val canAdd = name.isNotBlank() && path.isNotBlank()

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add a library") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(BeamSpacing.Small)) {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text("Name") },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                OutlinedTextField(
                    value = path,
                    onValueChange = { path = it },
                    label = { Text("Folder on the server") },
                    placeholder = { Text("/media/films") },
                    supportingText = {
                        Text("As the server sees it. It must sit inside the server's permitted library root.")
                    },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri),
                    modifier = Modifier.fillMaxWidth(),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onAdd(name.trim(), path.trim()) }, enabled = canAdd) { Text("Add") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun UserRow(
    user: AdminUser,
    onSetDisabled: (String, Boolean) -> Unit,
) {
    ListItem(
        headlineContent = { Text(user.displayName) },
        supportingContent = {
            Text(
                listOfNotNull(
                    user.email,
                    "Administrator".takeIf { user.isAdmin },
                    "Blocked".takeIf { user.disabled },
                ).joinToString(" · "),
            )
        },
        trailingContent = {
            TextButton(onClick = { onSetDisabled(user.id, !user.disabled) }) {
                Text(if (user.disabled) "Unblock" else "Block")
            }
        },
    )
}
