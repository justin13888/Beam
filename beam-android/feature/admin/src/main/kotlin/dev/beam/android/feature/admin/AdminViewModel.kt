package dev.beam.android.feature.admin

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.model.LoadState
import kotlinx.coroutines.async
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUser
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.LibrarySummary
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
    /** The result of the last action, to show in a snackbar. */
    val message: String? = null,
)

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
                                    runCatching { admin.users(limit = USER_PAGE, offset = null).items }
                                        .getOrDefault(emptyList())
                                }
                            AdminUiState(
                                status = status.await(),
                                libraries = libraries.await(),
                                users = users.await(),
                            )
                        }
                    mutableState.value = LoadState.Success(loaded)
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

        /** Rescan a library. */
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
                                    onSuccess = { added -> "Scan finished. $added files added." },
                                    onFailure = { error ->
                                        (error as? BeamException)?.toFailure()?.message
                                            ?: "The scan could not be started."
                                    },
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
                runCatching { admin.setUserDisabled(userId, disabled) }
                reload()
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
                runCatching { admin.deleteLibrary(libraryId) }
                reload()
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
        }
    }
