package dev.beam.android.feature.downloads

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.model.PlaybackRequest
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeoutOrNull
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.DownloadState
import uniffi.beam_client_core.MediaDetail
import javax.inject.Inject
import kotlin.time.Duration.Companion.seconds

/** Offline downloads, grouped by what the viewer can do with them. */
public data class DownloadsUiState(
    /** Finished, and playable with no network. */
    val completed: List<DownloadRecord> = emptyList(),
    /** Still moving, waiting, or paused. */
    val inProgress: List<DownloadRecord> = emptyList(),
    /** Stopped by an error. */
    val failed: List<DownloadRecord> = emptyList(),
) {
    /** Whether there is nothing at all. */
    public val isEmpty: Boolean
        get() = completed.isEmpty() && inProgress.isEmpty() && failed.isEmpty()
}

/** The downloads screen. */
@HiltViewModel
public class DownloadsViewModel
    @Inject
    constructor(
        private val downloads: DownloadRepository,
        private val catalog: CatalogRepository,
        private val playback: PlaybackRepository,
    ) : ViewModel() {
        public val state: StateFlow<DownloadsUiState> =
            downloads.downloads
                .map { records ->
                    DownloadsUiState(
                        completed = records.filter { it.state == DownloadState.COMPLETED },
                        inProgress =
                            records.filter {
                                it.state in
                                    setOf(
                                        DownloadState.QUEUED,
                                        DownloadState.DOWNLOADING,
                                        DownloadState.PAUSED,
                                        DownloadState.WAITING_FOR_NETWORK,
                                    )
                            },
                        failed = records.filter { it.state == DownloadState.FAILED },
                    )
                }
                // A download store that cannot be opened -- no session yet, or a
                // corrupt index -- shows an empty screen rather than crashing the tab
                // the viewer just switched to.
                .catch { emit(DownloadsUiState()) }
                .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DownloadsUiState())

        /** The resume lookup Play is waiting on, if any. */
        private var pendingPlay: Job? = null

        /** Stop a download, keeping what has been fetched. */
        public fun pause(fileId: String) {
            viewModelScope.launch { runCatching { downloads.pause(fileId) } }
        }

        /** Resume a paused download. */
        public fun resume(fileId: String) {
            viewModelScope.launch { runCatching { downloads.resume(fileId) } }
        }

        /** Delete a download and its bytes. */
        public fun remove(fileId: String) {
            viewModelScope.launch { runCatching { downloads.remove(fileId) } }
        }

        /** Delete everything. */
        public fun removeAll() {
            viewModelScope.launch { runCatching { downloads.removeAll() } }
        }

        /**
         * Play [record], resuming where the viewer stopped (issue #466), and
         * hand the request to [onReady].
         *
         * The position is the server's, through the core's start rule: the
         * title's own for a film, the episode's for a series. A downloaded file
         * is what plays with no network, so a lookup that fails or does not
         * answer within [RESUME_LOOKUP_TIMEOUT] starts from the last position
         * this device recorded for the file, through the same rule (issue
         * #487), rather than holding the player back -- or at 0 where the
         * device recorded none, or cannot read what it recorded.
         *
         * A tap while a lookup is still pending is ignored: each lookup ends
         * in a navigation, so a second tap would stack a second player.
         */
        public fun play(
            record: DownloadRecord,
            onReady: (PlaybackRequest) -> Unit,
        ) {
            if (pendingPlay?.isActive == true) return
            pendingPlay =
                viewModelScope.launch {
                    val detail =
                        withTimeoutOrNull(RESUME_LOOKUP_TIMEOUT) {
                            try {
                                catalog.detail(record.mediaId)
                            } catch (_: BeamException) {
                                null
                            }
                        }
                    onReady(
                        PlaybackRequest(
                            mediaId = record.mediaId,
                            episodeId = record.episodeId,
                            fileId = record.fileId,
                            title = record.title,
                            subtitle = record.subtitle,
                            artworkUrl = record.artworkUri(),
                            startPositionSecs =
                                detail?.startPositionFor(record.episodeId) ?: localStartPosition(record.fileId),
                        ),
                    )
                }
        }

        /** Where the core's own record puts [fileId]'s start, or 0 where it cannot be read. */
        private suspend fun localStartPosition(fileId: String): Double =
            try {
                playback.localStartPositionSecs(fileId)
            } catch (_: BeamException) {
                0.0
            }

        private companion object {
            /** How long Play waits on the server for a resume point before using the device's own. */
            val RESUME_LOOKUP_TIMEOUT = 3.seconds
        }
    }

/** Where the core says [episodeId] -- or, without one, the film itself -- starts. */
private fun MediaDetail.startPositionFor(episodeId: String?): Double =
    when (this) {
        is MediaDetail.Movie -> {
            summary.startPositionSecs
        }

        is MediaDetail.Show -> {
            seasons
                .asSequence()
                .flatMap { it.episodes }
                .firstOrNull { it.id == episodeId }
                ?.startPositionSecs
                ?: 0.0
        }
    }
