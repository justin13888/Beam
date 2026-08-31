package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
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
        onRetry = viewModel::refresh,
        modifier = modifier,
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
) {
    val content = state.valueOrNull

    when {
        content != null -> {
            AdminContent(
                state = content,
                onScan = onScan,
                onDeleteLibrary = onDeleteLibrary,
                onSetUserDisabled = onSetUserDisabled,
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
    modifier: Modifier = Modifier,
) {
    val status = state.status

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

        item(key = "libraries-header") { SectionHeader(title = "Libraries") }
        items(state.libraries, key = { it.id }) { library ->
            LibraryRow(
                library = library,
                isScanning = state.scanningLibraryId == library.id,
                onScan = { onScan(library.id) },
                onDelete = { onDeleteLibrary(library.id) },
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
    isScanning: Boolean,
    onScan: () -> Unit,
    onDelete: () -> Unit,
) {
    ListItem(
        headlineContent = { Text(library.name) },
        supportingContent = { Text("${library.size} files") },
        trailingContent = {
            if (isScanning) {
                CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
            } else {
                Row {
                    TextButton(onClick = onScan) { Text("Scan") }
                    TextButton(onClick = onDelete) { Text("Delete") }
                }
            }
        },
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
