package dev.beam.android.feature.admin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.EnrichmentStatus
import uniffi.beam_client_core.MatchCandidate
import uniffi.beam_client_core.MediaEnrichment
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MetadataField
import javax.inject.Inject

/**
 * Titles by enrichment status, newest change first: by default the unmatched
 * list, the titles waiting for an administrator to fix their match.
 */
@HiltViewModel
public class MetadataListViewModel
    @Inject
    constructor(
        private val admin: AdminRepository,
    ) : ViewModel() {
        private val mutableStatus = MutableStateFlow<EnrichmentStatus?>(EnrichmentStatus.UNMATCHED)

        /** The status listed; null lists every title. */
        public val status: StateFlow<EnrichmentStatus?> = mutableStatus.asStateFlow()

        private val mutableState = MutableStateFlow(PagedUiState<MediaEnrichment>())
        public val state: StateFlow<PagedUiState<MediaEnrichment>> = mutableState.asStateFlow()

        private val loader =
            PagedLoader(viewModelScope, mutableState, MediaEnrichment::mediaId) { after ->
                // Read when the page is asked for, so a page after a change of
                // filter is the new filter's, as the cursor requires.
                val page = admin.enrichment(mutableStatus.value, TITLE_PAGE, after)
                PagedLoader.Page(page.items, page.total, page.endCursor, page.hasNextPage)
            }

        init {
            addCloseable(loader.pager)
            loader.refresh()
        }

        /** List titles of [status] instead, or every title for null. */
        public fun setStatus(status: EnrichmentStatus?) {
            if (status == mutableStatus.value) return
            mutableStatus.value = status
            mutableState.update { it.copy(items = emptyList(), total = 0uL, hasNextPage = false) }
            loader.refresh()
        }

        /** Read the first page again. */
        public fun refresh(): Unit = loader.refresh()

        private var titlesClosedSeen: Int? = null

        /**
         * Read the first page again when [count], the Title pages closed over
         * the list so far, has moved since the list last looked: a fix-match
         * there moves the title out of the unmatched list. The first count seen
         * is covered by the list's own first read, and the same count again --
         * the list rebuilt after a rotation or a return from another tab --
         * reads nothing, keeping the pages already loaded.
         */
        public fun titlesClosed(count: Int) {
            val seen = titlesClosedSeen
            titlesClosedSeen = count
            if (seen != null && seen != count) loader.refresh()
        }

        /** Read the next page. */
        public fun loadMore(): Unit = loader.loadMore()

        private companion object {
            const val TITLE_PAGE: UInt = 50u
        }
    }

/** One title's matching and locks, as the screen for it renders them. */
public data class TitleMetadataUiState(
    /** Where the title's enrichment stands; null until it is read. */
    val enrichment: MediaEnrichment? = null,
    /** Why the title could not be read. */
    val error: String? = null,
    /** The candidates the last search found; null before any search. */
    val candidates: List<MatchCandidate>? = null,
    /** Whether a search is in flight. */
    val isSearching: Boolean = false,
    /** Why the last search failed. */
    val searchError: String? = null,
    /** The locks as the operator has ticked them, saved or not. */
    val locks: Set<MetadataField> = emptySet(),
    /** Whether a fix, a clear or a save is in flight. */
    val isSaving: Boolean = false,
    /** The result of the last action, for the snackbar. */
    val message: String? = null,
) {
    /** Whether the ticked locks differ from the ones the server holds. */
    public val hasUnsavedLocks: Boolean
        get() = enrichment != null && locks != enrichment.lockedFields.toSet()
}

/** Fix-match and field locks for one title. */
@HiltViewModel
public class TitleMetadataViewModel
    @Inject
    constructor(
        private val admin: AdminRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(TitleMetadataUiState())
        public val state: StateFlow<TitleMetadataUiState> = mutableState.asStateFlow()

        private var mediaId: String? = null

        /**
         * Show [id]'s metadata. Opening the title already shown reads nothing,
         * so a configuration change does not discard ticked locks.
         */
        public fun open(id: String) {
            if (id == mediaId) return
            mediaId = id
            mutableState.value = TitleMetadataUiState()
            read(id)
        }

        /** Read the title again after a read that failed, so it can be retried without leaving the page. */
        public fun retry() {
            val id = mediaId ?: return
            mutableState.update { it.copy(error = null) }
            read(id)
        }

        private fun read(id: String) {
            viewModelScope.launch {
                try {
                    show(admin.mediaEnrichment(id))
                } catch (failure: BeamException) {
                    mutableState.update { it.copy(error = failure.toFailure().message) }
                }
            }
        }

        /**
         * Search the providers for candidates. A blank [query] and a null
         * [year] search by the title's own.
         */
        public fun search(
            query: String,
            year: UInt?,
        ) {
            val id = mediaId ?: return
            mutableState.update { it.copy(isSearching = true, searchError = null) }
            viewModelScope.launch {
                try {
                    val found = admin.matchCandidates(id, query.trim().ifEmpty { null }, year)
                    mutableState.update { it.copy(candidates = found, isSearching = false) }
                } catch (failure: BeamException) {
                    mutableState.update { it.copy(isSearching = false, searchError = searchFailure(failure)) }
                }
            }
        }

        /** Fix the title's match to [candidate]; the worker fetches it by that id afterwards. */
        public fun fixMatch(candidate: MatchCandidate) {
            act(
                done = "Matched to ${candidate.title}. Its metadata is being fetched.",
                notDone = "The match could not be fixed.",
            ) {
                admin.fixMatch(it, candidate.externalRef)
            }
        }

        /** Clear the administrator's fixed match. */
        public fun clearMatch() {
            act(done = "Fixed match cleared.", notDone = "The match could not be cleared.") {
                admin.clearMatch(it)
                admin.mediaEnrichment(it)
            }
        }

        /** Tick or untick one lock; nothing is sent until [saveLocks]. */
        public fun toggleLock(field: MetadataField) {
            mutableState.update { it.copy(locks = if (field in it.locks) it.locks - field else it.locks + field) }
        }

        /** Send the ticked locks: exactly these fields are locked, every other one unlocked. */
        public fun saveLocks() {
            // In the order the screen lists them, so the request is stable.
            val fields = MetadataField.entries.filter { it in mutableState.value.locks }
            act(done = "Locks saved.", notDone = "The locks could not be saved.") {
                admin.setFieldLocks(it, fields)
            }
        }

        /** Dismiss the last message. */
        public fun clearMessage() {
            mutableState.update { it.copy(message = null) }
        }

        /** Run one change to the title, then show the title as the server answered it. */
        private fun act(
            done: String,
            notDone: String,
            change: suspend (mediaId: String) -> MediaEnrichment,
        ) {
            val id = mediaId ?: return
            mutableState.update { it.copy(isSaving = true) }
            viewModelScope.launch {
                try {
                    show(change(id))
                    mutableState.update { it.copy(isSaving = false, message = done) }
                } catch (failure: BeamException) {
                    val reason = failure.toFailure().message
                    mutableState.update { it.copy(isSaving = false, message = "$notDone $reason") }
                }
            }
        }

        private fun show(enrichment: MediaEnrichment) {
            mutableState.update {
                it.copy(
                    enrichment = enrichment,
                    locks = enrichment.lockedFields.toSet(),
                    error = null,
                )
            }
        }
    }

/** The fields a title of [kind] has to lock: a show has no release date or runtime of its own. */
internal fun lockableFields(kind: MediaKind): List<MetadataField> =
    when (kind) {
        MediaKind.MOVIE -> MetadataField.entries
        MediaKind.SHOW -> MetadataField.entries - setOf(MetadataField.RELEASE_DATE, MetadataField.RUNTIME)
    }

/**
 * Why a search found nothing to show. A server with no metadata provider
 * configured has nothing to search, which is a setup fact rather than a failure.
 */
private fun searchFailure(failure: BeamException): String =
    when {
        failure is BeamException.Server && failure.status == HTTP_CONFLICT.toUShort() -> {
            "No metadata provider is configured on the server, so there is nothing to search."
        }

        failure is BeamException.Server && failure.status == HTTP_BAD_GATEWAY.toUShort() -> {
            "The metadata provider did not answer. Try again shortly."
        }

        else -> {
            failure.toFailure().message
        }
    }

private const val HTTP_CONFLICT = 409
private const val HTTP_BAD_GATEWAY = 502
