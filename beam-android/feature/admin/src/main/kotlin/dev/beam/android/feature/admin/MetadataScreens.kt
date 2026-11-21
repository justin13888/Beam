package dev.beam.android.feature.admin

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.FilterChip
import androidx.compose.material3.ListItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import dev.beam.android.core.designsystem.BeamSpacing
import uniffi.beam_client_core.EnrichmentStatus
import uniffi.beam_client_core.MediaEnrichment
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MetadataField
import uniffi.beam_client_core.PinSource
import kotlin.math.roundToInt

/** The filters the metadata list offers, the unmatched list first. */
internal val statusFilters: List<Pair<EnrichmentStatus?, String>> =
    listOf(
        EnrichmentStatus.UNMATCHED to "Unmatched",
        EnrichmentStatus.FAILED to "Failed",
        EnrichmentStatus.PENDING to "Pending",
        EnrichmentStatus.ENRICHED to "Enriched",
        null to "All",
    )

/** Titles by enrichment status; a tap opens a title's fix-match and locks. */
@Composable
internal fun MetadataListScreen(
    state: PagedUiState<MediaEnrichment>,
    status: EnrichmentStatus?,
    onStatus: (EnrichmentStatus?) -> Unit,
    onBack: () -> Unit,
    onRefresh: () -> Unit,
    onLoadMore: () -> Unit,
    onOpenTitle: (mediaId: String, title: String) -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.fillMaxSize()) {
        PageHeader(title = "Metadata", onBack = onBack, onRefresh = onRefresh)
        LazyRow(
            contentPadding = PaddingValues(horizontal = BeamSpacing.Medium),
            horizontalArrangement = Arrangement.spacedBy(BeamSpacing.Small),
        ) {
            items(statusFilters, key = { it.second }) { (option, label) ->
                FilterChip(selected = option == status, onClick = { onStatus(option) }, label = { Text(label) })
            }
        }
        Box(modifier = Modifier.fillMaxSize()) {
            PagedList(
                state = state,
                empty = "No titles have this status.",
                key = MediaEnrichment::mediaId,
                onRetry = onRefresh,
                onLoadMore = onLoadMore,
            ) { title ->
                ListItem(
                    overlineContent = { Text(enrichmentOverline(title)) },
                    headlineContent = {
                        Text(
                            listOfNotNull(title.title, title.year?.let { "($it)" }).joinToString(" "),
                        )
                    },
                    supportingContent = enrichmentDetail(title)?.let { detail -> { Text(detail) } },
                    modifier = Modifier.fillMaxWidth().clickable { onOpenTitle(title.mediaId, title.title) },
                )
            }
        }
    }
}

/** A title's kind and where its enrichment stands, in a line. */
internal fun enrichmentOverline(enrichment: MediaEnrichment): String {
    val kind =
        when (enrichment.kind) {
            MediaKind.MOVIE -> "Movie"
            MediaKind.SHOW -> "Show"
        }
    return "$kind · ${statusLabel(enrichment.status)}"
}

/**
 * What an operator needs to act on a title: why it is not enriched, what it
 * is matched or pinned to, and which fields are locked. Null when there is
 * nothing beyond its status to say.
 */
internal fun enrichmentDetail(enrichment: MediaEnrichment): String? =
    listOfNotNull(
        enrichment.lastError,
        enrichment.pinnedRef?.let { ref -> "Pinned to $ref by ${pinSourceLabel(enrichment.pinSource)}" },
        enrichment.matchedRef?.takeIf { it != enrichment.pinnedRef }?.let { ref ->
            "Matched to $ref" + (enrichment.matchConfidence?.let { " (${(it * 100).roundToInt()}% sure)" } ?: "")
        },
        enrichment.lockedFields
            .takeIf { it.isNotEmpty() }
            ?.joinToString(", ", prefix = "Locked: ") { fieldLabel(it).lowercase() },
    ).joinToString("\n").ifEmpty { null }

internal fun statusLabel(status: EnrichmentStatus): String =
    when (status) {
        EnrichmentStatus.PENDING -> "Pending"
        EnrichmentStatus.ENRICHED -> "Enriched"
        EnrichmentStatus.UNMATCHED -> "Unmatched"
        EnrichmentStatus.FAILED -> "Failed"
    }

private fun pinSourceLabel(source: PinSource?): String =
    when (source) {
        PinSource.ADMIN -> "an administrator"
        PinSource.NFO -> "an NFO file"
        null -> "an unknown source"
    }

/** A metadata field as the lock list names it. */
internal fun fieldLabel(field: MetadataField): String =
    when (field) {
        MetadataField.TITLE -> "Title"
        MetadataField.ORIGINAL_TITLE -> "Original title"
        MetadataField.DESCRIPTION -> "Description"
        MetadataField.YEAR -> "Year"
        MetadataField.RELEASE_DATE -> "Release date"
        MetadataField.RUNTIME -> "Runtime"
        MetadataField.POSTER -> "Poster"
        MetadataField.BACKDROP -> "Backdrop"
        MetadataField.RATING -> "Rating"
        MetadataField.GENRES -> "Genres"
    }
