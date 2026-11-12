package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import dev.beam.android.core.designsystem.BeamSpacing
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.ScanJob

@Composable
internal fun StatCard(
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
internal fun LibraryRow(
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

@Composable
internal fun AddLibraryDialog(
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
internal fun UserRow(
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
