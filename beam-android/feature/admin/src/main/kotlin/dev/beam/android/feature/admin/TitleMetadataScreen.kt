package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedButton
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
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.BeamLoading
import dev.beam.android.core.designsystem.component.SectionHeader
import uniffi.beam_client_core.MatchCandidate
import uniffi.beam_client_core.MetadataField
import uniffi.beam_client_core.PinSource
import kotlin.math.roundToInt

/** One title's fix-match -- a search, then a pick -- and its field locks. */
@Composable
internal fun TitleMetadataScreen(
    title: String,
    state: TitleMetadataUiState,
    onBack: () -> Unit,
    onSearch: (query: String, year: UInt?) -> Unit,
    onFixMatch: (MatchCandidate) -> Unit,
    onClearMatch: () -> Unit,
    onToggleLock: (MetadataField) -> Unit,
    onSaveLocks: () -> Unit,
    modifier: Modifier = Modifier,
    onMessageShown: () -> Unit = {},
) {
    val snackbarHost = remember { SnackbarHostState() }
    LaunchedEffect(state.message) {
        state.message?.let { message ->
            snackbarHost.showSnackbar(message)
            onMessageShown()
        }
    }
    var picking by remember { mutableStateOf<MatchCandidate?>(null) }
    picking?.let { candidate ->
        // Fixing a match replaces the title's metadata with the candidate's,
        // so it is confirmed rather than one tap on a search result.
        AlertDialog(
            onDismissRequest = { picking = null },
            title = { Text("Match to ${candidate.title}?") },
            text = {
                Text(
                    "Beam fetches ${candidate.externalRef} for this title and keeps that match until it is cleared.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    picking = null
                    onFixMatch(candidate)
                }) { Text("Match") }
            },
            dismissButton = { TextButton(onClick = { picking = null }) { Text("Cancel") } },
        )
    }

    Box(modifier = modifier.fillMaxSize()) {
        Column(modifier = Modifier.fillMaxSize()) {
            PageHeader(title = title, onBack = onBack)
            if (state.isSaving) LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
            val enrichment = state.enrichment
            when {
                enrichment != null -> {
                    TitleMetadataContent(state, enrichment, onSearch, onClearMatch, onToggleLock, onSaveLocks) {
                        picking =
                            it
                    }
                }

                state.error != null -> {
                    BeamErrorState(message = state.error)
                }

                else -> {
                    BeamLoading()
                }
            }
        }
        SnackbarHost(hostState = snackbarHost, modifier = Modifier.align(Alignment.BottomCenter))
    }
}

@Composable
private fun TitleMetadataContent(
    state: TitleMetadataUiState,
    enrichment: uniffi.beam_client_core.MediaEnrichment,
    onSearch: (query: String, year: UInt?) -> Unit,
    onClearMatch: () -> Unit,
    onToggleLock: (MetadataField) -> Unit,
    onSaveLocks: () -> Unit,
    onPick: (MatchCandidate) -> Unit,
) {
    var query by rememberSaveable { mutableStateOf("") }
    var year by rememberSaveable { mutableStateOf("") }
    LazyColumn(contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge)) {
        item(key = "status") {
            ListItem(
                overlineContent = { Text(enrichmentOverline(enrichment)) },
                headlineContent = { Text(enrichment.title) },
                supportingContent = enrichmentDetail(enrichment)?.let { detail -> { Text(detail) } },
            )
        }
        if (enrichment.pinSource == PinSource.ADMIN) {
            item(key = "clear") {
                OutlinedButton(
                    onClick = onClearMatch,
                    enabled = !state.isSaving,
                    modifier = Modifier.padding(horizontal = BeamSpacing.Medium),
                ) { Text("Clear fixed match") }
            }
        }

        item(key = "search-header") { SectionHeader(title = "Fix match") }
        item(key = "search") {
            Column(
                modifier = Modifier.padding(horizontal = BeamSpacing.Medium),
                verticalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
            ) {
                OutlinedTextField(
                    value = query,
                    onValueChange = { query = it },
                    label = { Text("Search") },
                    placeholder = { Text(enrichment.title) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth(),
                )
                Row(
                    horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    OutlinedTextField(
                        value = year,
                        onValueChange = { typed -> year = typed.filter(Char::isDigit).take(YEAR_DIGITS) },
                        label = { Text("Year") },
                        placeholder = { Text(enrichment.year?.toString() ?: "") },
                        singleLine = true,
                        keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                        modifier = Modifier.weight(1f),
                    )
                    Button(onClick = { onSearch(query, year.toUIntOrNull()) }, enabled = !state.isSearching) {
                        Text("Search")
                    }
                }
            }
        }
        if (state.isSearching) item(key = "searching") { LinearProgressIndicator(modifier = Modifier.fillMaxWidth()) }
        state.searchError?.let { error -> item(key = "search-error") { ListItem(headlineContent = { Text(error) }) } }
        state.candidates?.let { candidates ->
            if (candidates.isEmpty()) {
                item(
                    key = "no-candidates",
                ) { ListItem(headlineContent = { Text("No candidates. Try other words or a year.") }) }
            }
            items(candidates, key = { it.externalRef }) { candidate ->
                ListItem(
                    headlineContent = {
                        Text(
                            listOfNotNull(candidate.title, candidate.year?.let { "($it)" }).joinToString(" "),
                        )
                    },
                    supportingContent = { Text(candidateDetail(candidate)) },
                    trailingContent = {
                        TextButton(onClick = { onPick(candidate) }, enabled = !state.isSaving) { Text("Pick") }
                    },
                )
            }
        }

        item(key = "locks-header") {
            SectionHeader(
                title = "Locked fields",
                actionLabel = "Save".takeIf { state.hasUnsavedLocks && !state.isSaving },
                onAction = onSaveLocks.takeIf { state.hasUnsavedLocks && !state.isSaving },
            )
        }
        items(lockableFields(enrichment.kind), key = { it.name }) { field ->
            ListItem(
                headlineContent = { Text(fieldLabel(field)) },
                trailingContent = {
                    Checkbox(
                        checked = field in state.locks,
                        onCheckedChange = { onToggleLock(field) },
                    )
                },
            )
        }
    }
}

/** A candidate's provider reference, original title and how well it scores. */
internal fun candidateDetail(candidate: MatchCandidate): String =
    listOfNotNull(
        candidate.externalRef,
        candidate.originalTitle?.takeIf { it != candidate.title },
        "${(candidate.score.coerceIn(0.0, 1.0) * 100).roundToInt()}% match",
    ).joinToString(" · ")

private const val YEAR_DIGITS = 4
