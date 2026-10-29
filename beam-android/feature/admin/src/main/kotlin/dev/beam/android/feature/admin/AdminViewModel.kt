package dev.beam.android.feature.admin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.model.LoadState
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.PlaybackTelemetrySummary
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ScanJobState
import java.time.LocalDate
import java.time.ZoneOffset
import javax.inject.Inject

/** The administrative dashboard. */
public data class AdminUiState(
    /** The server's own snapshot. */
    val status: AdminStatus,
    /** Every library, for scanning and deletion. */
    val libraries: List<LibrarySummary> = emptyList(),
    /** User accounts. */
    val users: List<AdminUser> = emptyList(),
    /** A library currently being scanned, so the row can say so. */
    val scanningLibraryId: String? = null,
    /**
     * Each library's latest scan since the server started, keyed by library
     * id. A library with no entry has not been scanned since, or its scan
     * could not be read; either way its row shows its last finished scan.
     */
    val scans: Map<String, ScanJob> = emptyMap(),
    /** The result of the last action, to show in a snackbar. */
    val message: String? = null,
    /** The days playback telemetry is shown for. */
    val telemetryRange: TelemetryRange = TelemetryRange.LAST_30_DAYS,
    /** Playback telemetry for [telemetryRange], or null before it loads or when it failed. */
    val telemetry: PlaybackTelemetrySummary? = null,
    /**
     * Why telemetry could not be loaded. A section of its own, so its failing
     * never takes the rest of the dashboard with it.
     */
    val telemetryError: String? = null,
)

/** The ranges of days the playback telemetry section offers. */
public enum class TelemetryRange(
    /** How many days, today included. */
    public val days: Long,
    /** What the picker shows. */
    public val label: String,
) {
    LAST_7_DAYS(7, "7 days"),
    LAST_30_DAYS(30, "30 days"),
    LAST_90_DAYS(90, "90 days"),
    LAST_365_DAYS(365, "1 year"),
    ;

    /**
     * The `from` and `to` days, `YYYY-MM-DD`, ending on [today]. The server
     * counts in UTC days, so [today] is the UTC date.
     */
    public fun bounds(today: LocalDate): Pair<String, String> = today.minusDays(days - 1).toString() to today.toString()
}

/**
 * The admin area.
 *
 * Guarded by the server, not by this class: every call here is one the server
 * rejects with 403 for a non-administrator, and the core surfaces that as
 * [BeamException.Forbidden]. Hiding the screen is a courtesy, never the
 * control.
 */
@HiltViewModel
public class AdminViewModel
    @Inject
    constructor(
        private val admin: AdminRepository,
        private val catalog: CatalogRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow<LoadState<AdminUiState>>(LoadState.Idle)
        public val state: StateFlow<LoadState<AdminUiState>> = mutableState.asStateFlow()

        init {
            refresh()
        }

        /** Reload the dashboard. */
        public fun refresh() {
            viewModelScope.launch { reload() }
        }

        /**
         * The reload itself, so an action can await it.
         *
         * Load-bearing: `refresh` returns as soon as it has launched, so an
         * action that set a confirmation message and then called it would have
         * the reload overwrite the message it had just written.
         */
        private suspend fun reload() {
            mutableState.value = LoadState.Loading(mutableState.value.previous())
            run {
                try {
                    val loaded =
                        coroutineScope {
                            val status = async { admin.status() }
                            val libraries = async { catalog.libraries() }
                            val users =
                                async {
                                    runCatching { admin.users(first = USER_PAGE, after = null).items }
                                        .getOrDefault(emptyList())
                                }
                            val range = mutableState.value.previous()?.telemetryRange ?: TelemetryRange.LAST_30_DAYS
                            val telemetry = async { loadTelemetry(range) }
                            val (summary, telemetryError) = telemetry.await()
                            val libraryList = libraries.await()
                            AdminUiState(
                                status = status.await(),
                                libraries = libraryList,
                                users = users.await(),
                                scans = loadScans(libraryList.map { it.id }),
                                telemetryRange = range,
                                telemetry = summary,
                                telemetryError = telemetryError,
                            )
                        }
                    mutableState.value = LoadState.Success(loaded)
                    followRunningScans()
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

        /**
         * Start a rescan of a library. The server answers once it has accepted
         * the scan, not when the scan finishes: its progress reaches the event
         * feed, and the library's counts on the next reload.
         */
        public fun scan(libraryId: String) {
            mutableState.update { it.mapValue { state -> state.copy(scanningLibraryId = libraryId) } }
            viewModelScope.launch {
                val outcome = runCatching { admin.scanLibrary(libraryId) }
                // Reloaded first, message applied after: `reload` replaces the
                // whole state, so writing the message before it would have the
                // reload wipe the confirmation the operator needs to see.
                reload()
                mutableState.update {
                    it.mapValue { state ->
                        state.copy(
                            scanningLibraryId = null,
                            message =
                                outcome.fold(
                                    onSuccess = { "Scan started." },
                                    onFailure = { error -> scanFailureMessage(error) },
                                ),
                        )
                    }
                }
            }
        }

        /** Block or unblock an account. */
        public fun setUserDisabled(
            userId: String,
            disabled: Boolean,
        ) {
            viewModelScope.launch {
                val outcome = runCatching { admin.setUserDisabled(userId, disabled) }
                reload()
                // Only a failure is announced: a block that worked shows in the
                // row itself, and one that was refused would otherwise look
                // exactly like the operator never pressed it. Applied after the
                // reload, which replaces the whole state.
                outcome.onFailure { error ->
                    val notDone =
                        if (disabled) "The account could not be blocked." else "The account could not be unblocked."
                    showMessage(mutationFailureMessage(error, notDone))
                }
            }
        }

        /** Add a library from a path on the server. */
        public fun createLibrary(
            name: String,
            rootPath: String,
        ) {
            viewModelScope.launch {
                val outcome = runCatching { admin.createLibrary(name, rootPath) }
                reload()
                mutableState.update {
                    it.mapValue { state ->
                        state.copy(
                            message =
                                outcome.fold(
                                    onSuccess = { library -> "Added ${library.name}." },
                                    onFailure = { error ->
                                        (error as? BeamException)?.toFailure()?.message
                                            ?: "The library could not be added."
                                    },
                                ),
                        )
                    }
                }
            }
        }

        /** Delete a library and everything indexed into it. */
        public fun deleteLibrary(libraryId: String) {
            viewModelScope.launch {
                val outcome = runCatching { admin.deleteLibrary(libraryId) }
                reload()
                outcome.onFailure { error ->
                    showMessage(mutationFailureMessage(error, "The library could not be deleted."))
                }
            }
        }

        /** Put [message] in the snackbar, over whatever is on screen. */
        private fun showMessage(message: String) {
            mutableState.update { it.mapValue { state -> state.copy(message = message) } }
        }

        /** Show playback telemetry for a different range of days. */
        public fun setTelemetryRange(range: TelemetryRange) {
            mutableState.update { it.mapValue { state -> state.copy(telemetryRange = range) } }
            viewModelScope.launch {
                val (summary, error) = loadTelemetry(range)
                mutableState.update {
                    it.mapValue { state ->
                        // A later pick wins: an answer for a range no longer
                        // selected is about a question nobody is asking.
                        if (state.telemetryRange != range) {
                            state
                        } else {
                            state.copy(telemetry = summary, telemetryError = error)
                        }
                    }
                }
            }
        }

        /**
         * The latest scan of each of [libraryIds], read side by side. A scan
         * that cannot be read is left out rather than failing the dashboard:
         * its row falls back to the library's last finished scan.
         */
        private suspend fun loadScans(libraryIds: List<String>): Map<String, ScanJob> =
            coroutineScope {
                libraryIds
                    .map { id -> async { id to runCatching { admin.scanJob(id) }.getOrNull() } }
                    .awaitAll()
                    .mapNotNull { (id, job) -> job?.let { id to it } }
                    .toMap()
            }

        /** The poll following a queued or running scan, while one is. */
        private var scanFollower: Job? = null

        /**
         * Keep a queued or running scan's progress current until it finishes,
         * so the operator watches it advance rather than pressing refresh.
         *
         * Bounded: a scan still running after [MAX_SCAN_POLLS] reads is left
         * for the operator to refresh, rather than polled for as long as the
         * screen is open. A finished scan changes its library's file count, so
         * the libraries are read again then.
         */
        private fun followRunningScans() {
            if (scanFollower?.isActive == true) return
            scanFollower =
                viewModelScope.launch {
                    repeat(MAX_SCAN_POLLS) {
                        val following =
                            mutableState.value
                                .previous()
                                ?.scans
                                ?.values
                                ?.filter { it.state.isUnderWay() }
                                .orEmpty()
                        if (following.isEmpty()) return@launch
                        delay(SCAN_POLL_MS)
                        val updated = loadScans(following.map { it.libraryId })
                        val libraries =
                            if (updated.values.any { !it.state.isUnderWay() }) {
                                runCatching { catalog.libraries() }.getOrNull()
                            } else {
                                null
                            }
                        mutableState.update {
                            it.mapValue { state ->
                                state.copy(
                                    scans = state.scans + updated,
                                    libraries = libraries ?: state.libraries,
                                )
                            }
                        }
                    }
                }
        }

        /** Telemetry for [range], or why it could not be had. */
        private suspend fun loadTelemetry(range: TelemetryRange): Pair<PlaybackTelemetrySummary?, String?> {
            val (from, to) = range.bounds(LocalDate.now(ZoneOffset.UTC))
            return try {
                admin.playbackTelemetry(from, to) to null
            } catch (failure: BeamException) {
                null to failure.toFailure().message
            }
        }

        /** Dismiss the last message. */
        public fun clearMessage() {
            mutableState.update { it.mapValue { state -> state.copy(message = null) } }
        }

        private fun LoadState<AdminUiState>.previous(): AdminUiState? =
            when (this) {
                is LoadState.Success -> value
                is LoadState.Loading -> previous
                is LoadState.Failure -> previous
                LoadState.Idle -> null
            }

        /**
         * Apply a change to whichever value is currently on screen.
         *
         * The retained `previous` of a Loading or Failure is mapped too,
         * because that is what the screen is still rendering. Mapping only
         * Success leaves a scan's progress indicator spinning forever whenever
         * the reload after it also fails -- indistinguishable, to the
         * operator, from a scan that never completes.
         */
        private fun LoadState<AdminUiState>.mapValue(
            transform: (AdminUiState) -> AdminUiState,
        ): LoadState<AdminUiState> =
            when (this) {
                is LoadState.Success -> {
                    LoadState.Success(transform(value))
                }

                is LoadState.Loading -> {
                    LoadState.Loading(previous?.let(transform))
                }

                is LoadState.Failure -> {
                    LoadState.Failure(message, retryable, previous?.let(transform))
                }

                LoadState.Idle -> {
                    this
                }
            }

        private companion object {
            const val USER_PAGE: UInt = 100u

            /** How often a scan under way is read again. */
            const val SCAN_POLL_MS: Long = 2_000L

            /** How many reads a scan is followed for: fifteen minutes' worth. */
            const val MAX_SCAN_POLLS: Int = 450
        }
    }

/** Whether a scan is waiting or running, as opposed to finished either way. */
internal fun ScanJobState.isUnderWay(): Boolean = this == ScanJobState.QUEUED || this == ScanJobState.RUNNING

/**
 * That an action was not carried out, and why where the core mapped a reason:
 * [notDone] names the action, so the operator knows which press failed.
 */
private fun mutationFailureMessage(
    error: Throwable,
    notDone: String,
): String = listOfNotNull(notDone, (error as? BeamException)?.toFailure()?.message).joinToString(" ")

/** The problem type the server answers a scan of a library already being scanned with. */
private const val SCAN_IN_PROGRESS = "#library-scan-in-progress"

/**
 * Why a scan could not be started, for the operator. A scan already running
 * is not a failure of anything: the library is being scanned.
 */
private fun scanFailureMessage(error: Throwable): String =
    when {
        error is BeamException.Server && error.code.endsWith(SCAN_IN_PROGRESS) -> {
            "A scan of this library is already running."
        }

        else -> {
            (error as? BeamException)?.toFailure()?.message
                ?: "The scan could not be started."
        }
    }
