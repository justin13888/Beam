package dev.beam.android.feature.detail

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.model.LoadState
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.MediaDetail
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.Playability
import uniffi.beam_client_core.SourceSelection
import javax.inject.Inject

/** Everything one title's page shows. */
public data class DetailUiState(
    /** The title itself. */
    val detail: MediaDetail,
    /** Which season's episodes are on screen. */
    val selectedSeason: UInt? = null,
    /** Every file behind the title, playable or not. */
    val sources: List<MediaSourceView> = emptyList(),
    /** The file that would play, and why. */
    val selection: SourceSelection? = null,
    /** Whether the source picker is open. */
    val isPickingSource: Boolean = false,
    /** Files already downloaded, so the action reads "Play offline". */
    val downloadedFileIds: Set<String> = emptySet(),
) {
    /** The seasons, for a series; empty for a film. */
    public val seasons: List<uniffi.beam_client_core.SeasonSummary>
        get() = (detail as? MediaDetail.Show)?.seasons.orEmpty()

    /** The episodes of the selected season. */
    public val episodes: List<EpisodeSummary>
        get() = seasons.firstOrNull { it.seasonNumber == selectedSeason }?.episodes.orEmpty()

    /** The title's own summary, whichever kind it is. */
    public val summary: uniffi.beam_client_core.MediaSummary
        get() =
            when (detail) {
                is MediaDetail.Movie -> detail.summary
                is MediaDetail.Show -> detail.summary
            }

    /**
     * Whether the chosen file plays only in software.
     *
     * Worth saying out loud: software decoding of a 4K HEVC stream will drain
     * the battery and may drop frames, and a viewer who knows that can pick a
     * smaller file instead of concluding the app is broken.
     */
    public val isSoftwareOnly: Boolean
        get() = selection?.playability is Playability.Software
}

/**
 * One title's page.
 *
 * The source list is loaded alongside the detail rather than when the picker
 * opens, because the *play* action needs it: which file plays is a per-device
 * decision the core makes from the capability profile, and it has to be
 * settled before the button does anything.
 */
@HiltViewModel
public class DetailViewModel
    @Inject
    constructor(
        private val catalog: CatalogRepository,
        private val playback: PlaybackRepository,
        private val downloads: DownloadRepository,
        private val preferences: PreferencesRepository,
        savedStateHandle: SavedStateHandle,
    ) : ViewModel() {
        private val mediaId: String =
            requireNotNull(savedStateHandle["mediaId"]) {
                "the detail screen needs a mediaId"
            }

        private val mutableState = MutableStateFlow<LoadState<DetailUiState>>(LoadState.Idle)
        public val state: StateFlow<LoadState<DetailUiState>> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Reload the title. */
        public fun refresh() {
            mutableState.value = LoadState.Loading(mutableState.value.previous())
            viewModelScope.launch {
                try {
                    val detail = catalog.detail(mediaId)
                    // Sources are fetched separately and are allowed to fail: a
                    // title whose files cannot be listed is still worth showing,
                    // and the page degrades to "unavailable" rather than to an
                    // error page with no information on it at all.
                    val sources = runCatching { playback.sources(mediaId) }.getOrDefault(emptyList())
                    val policy =
                        preferences.preferences
                            .first()
                            .quality
                            .asPolicy()
                    val selection =
                        runCatching {
                            playback.selectSource(mediaId, policy)
                        }.getOrNull()

                    mutableState.value =
                        LoadState.Success(
                            DetailUiState(
                                detail = detail,
                                selectedSeason =
                                    (detail as? MediaDetail.Show)
                                        ?.seasons
                                        ?.firstOrNull()
                                        ?.seasonNumber,
                                sources = sources,
                                selection = selection,
                            ),
                        )
                } catch (failure: BeamException) {
                    val reason = failure.toFailure()
                    mutableState.value =
                        LoadState.Failure(
                            reason.message,
                            reason.retryable,
                            mutableState.value.previous(),
                        )
                }
            }
        }

        /** Show a different season's episodes. */
        public fun selectSeason(seasonNumber: UInt) {
            mutableState.update { current ->
                current.mapValue { it.copy(selectedSeason = seasonNumber) }
            }
        }

        /** Open or close the source picker. */
        public fun setPickingSource(open: Boolean) {
            mutableState.update { current ->
                current.mapValue { it.copy(isPickingSource = open) }
            }
        }

        /** Queue a file for offline playback. */
        public fun download(
            fileId: String,
            serverId: String,
            title: String,
            subtitle: String?,
        ) {
            viewModelScope.launch {
                runCatching {
                    downloads.enqueue(
                        fileId = fileId,
                        serverId = serverId,
                        mediaId = mediaId,
                        title = title,
                        subtitle = subtitle,
                        posterUrl =
                            mutableState.value
                                .previous()
                                ?.summary
                                ?.posterUrl,
                    )
                }
            }
        }

        private fun LoadState<DetailUiState>.previous(): DetailUiState? =
            when (this) {
                is LoadState.Success -> value
                is LoadState.Loading -> previous
                is LoadState.Failure -> previous
                LoadState.Idle -> null
            }

        private fun LoadState<DetailUiState>.mapValue(
            transform: (DetailUiState) -> DetailUiState,
        ): LoadState<DetailUiState> =
            when (this) {
                is LoadState.Success -> LoadState.Success(transform(value))
                else -> this
            }
    }
