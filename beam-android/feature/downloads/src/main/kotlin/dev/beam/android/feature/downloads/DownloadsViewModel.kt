package dev.beam.android.feature.downloads

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import javax.inject.Inject

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
    ) : ViewModel() {
        public val state: StateFlow<DownloadsUiState> =
            downloads.downloads
                .map { records ->
                    DownloadsUiState(
                        completed = records.filter { it.state == DownloadState.Completed },
                        inProgress =
                            records.filter {
                                it.state in
                                    setOf(
                                        DownloadState.Queued,
                                        DownloadState.Downloading,
                                        DownloadState.Paused,
                                        DownloadState.WaitingForNetwork,
                                    )
                            },
                        failed = records.filter { it.state == DownloadState.Failed },
                    )
                }
                // A download store that cannot be opened -- no session yet, or a
                // corrupt index -- shows an empty screen rather than crashing the tab
                // the viewer just switched to.
                .catch { emit(DownloadsUiState()) }
                .stateIn(viewModelScope, SharingStarted.WhileSubscribed(5_000), DownloadsUiState())

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
    }
