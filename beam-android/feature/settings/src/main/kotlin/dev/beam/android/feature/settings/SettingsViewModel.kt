package dev.beam.android.feature.settings

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.repository.SessionRepository
import dev.beam.android.core.ffi.toFailure
import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode
import dev.beam.android.core.model.UserPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.DeviceSession
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState
import uniffi.beam_client_core.UserSummary
import javax.inject.Inject

/** Everything the settings screen shows. */
public data class SettingsUiState(
    /** The current preferences. */
    val preferences: UserPreferences = UserPreferences(),
    /** The active server. */
    val server: ServerSummary? = null,
    /** Who is signed in. */
    val user: UserSummary? = null,
    /** Other devices signed in as this user. */
    val sessions: List<DeviceSession> = emptyList(),
    /** Certificates accepted for the active server. */
    val trustedCertificates: List<String> = emptyList(),
    /** Set once the viewer has signed out, so the shell can navigate. */
    val isSignedOut: Boolean = false,
    /** Every server this device knows, the active one among them. */
    val servers: List<ServerSummary> = emptyList(),
    /**
     * Set once the viewer has switched to a server they are signed in to, so
     * the shell can start the app afresh against it. A switch to one they are
     * not signed in to sets [isSignedOut] instead: signing in comes first.
     */
    val hasSwitchedServer: Boolean = false,
    /** Why the last action the viewer asked for did not happen, until dismissed. */
    val error: String? = null,
)

/** Preferences, the account, and this device's trust decisions. */
@HiltViewModel
public class SettingsViewModel
    @Inject
    constructor(
        private val preferences: PreferencesRepository,
        private val servers: ServerRepository,
        private val sessions: SessionRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(SettingsUiState())
        public val state: StateFlow<SettingsUiState> = mutableState.asStateFlow()

        init {
            viewModelScope.launch {
                preferences.preferences.collect { value ->
                    mutableState.update { it.copy(preferences = value) }
                }
            }
            viewModelScope.launch {
                servers.servers.collect { known ->
                    mutableState.update { it.copy(servers = known) }
                }
            }
            refresh()
        }

        /**
         * Make another known server the active one.
         *
         * Everything on screen belongs to the server it was loaded from, so a
         * switch is a fresh start: home, against the new server, when the
         * viewer is signed in there; sign-in first when they are not.
         */
        public fun switchServer(serverId: String) {
            if (mutableState.value.server?.id == serverId) return
            viewModelScope.launch {
                runCatching { servers.selectServer(serverId) }.onFailure { return@launch }
                val chosen = runCatching { servers.activeServer() }.getOrNull()
                if (chosen?.state is SessionState.Authenticated) {
                    mutableState.update { it.copy(hasSwitchedServer = true) }
                } else {
                    mutableState.update { it.copy(isSignedOut = true) }
                }
            }
        }

        /**
         * Forget a server this device is not using, with its session and its
         * queued progress. The active one is signed out of instead, which is
         * the account row's button.
         */
        public fun removeServer(serverId: String) {
            if (mutableState.value.server?.id == serverId) return
            viewModelScope.launch {
                runCatching { servers.removeServer(serverId) }
            }
        }

        /** Reload the account and server sections. */
        public fun refresh() {
            viewModelScope.launch {
                val server = runCatching { servers.activeServer() }.getOrNull()
                val user = (server?.state as? SessionState.Authenticated)?.user
                val devices = runCatching { sessions.sessions() }.getOrDefault(emptyList())
                val certificates =
                    server
                        ?.let { runCatching { servers.trustedCertificates(it.id) }.getOrNull() }
                        .orEmpty()

                mutableState.update {
                    it.copy(
                        server = server,
                        user = user,
                        sessions = devices,
                        trustedCertificates = certificates,
                    )
                }
            }
        }

        /** Change the colour scheme. */
        public fun setThemeMode(mode: ThemeMode): Unit = edit { it.copy(themeMode = mode) }

        /** Change where the palette comes from. */
        public fun setPaletteSource(source: PaletteSource): Unit = edit { it.copy(paletteSource = source) }

        /** Change which source the player reaches for first. */
        public fun setQuality(quality: QualityPreference): Unit = edit { it.copy(quality = quality) }

        /** Turn auto-advance on or off. */
        public fun setAutoPlayNext(enabled: Boolean): Unit = edit { it.copy(autoPlayNext = enabled) }

        /** Allow or forbid software decoding. */
        public fun setAllowSoftwareDecode(enabled: Boolean): Unit = edit { it.copy(allowSoftwareDecode = enabled) }

        /** Allow or forbid downloads without Wi-Fi. */
        public fun setDownloadOverCellular(enabled: Boolean): Unit = edit { it.copy(downloadOverCellular = enabled) }

        /** Revoke one signed-in device. */
        public fun revokeSession(sessionId: String) {
            viewModelScope.launch {
                runCatching { sessions.revoke(sessionId) }
                refresh()
            }
        }

        /**
         * Withdraw trust from every certificate accepted for the active server.
         *
         * Offered because a trust decision made once, possibly hastily, should be
         * reversible without reinstalling the app.
         */
        public fun forgetCertificates() {
            val serverId = mutableState.value.server?.id ?: return
            viewModelScope.launch {
                runCatching { servers.forgetCertificates(serverId) }
                refresh()
            }
        }

        /** Sign out of this device. */
        public fun signOut() {
            val serverId = mutableState.value.server?.id ?: return
            viewModelScope.launch {
                runCatching { servers.logout(serverId) }
                mutableState.update { it.copy(isSignedOut = true) }
            }
        }

        /**
         * End every session everywhere, including this one.
         *
         * A failure is reported, not taken as done: the viewer asked for the
         * other devices to be signed out, and leaving the screen as though
         * they were would be a security-relevant lie. This device stays signed
         * in so they can try again.
         */
        public fun signOutEverywhere() {
            mutableState.update { it.copy(error = null) }
            viewModelScope.launch {
                try {
                    sessions.logoutEverywhere()
                    mutableState.update { it.copy(isSignedOut = true) }
                } catch (failure: BeamException) {
                    mutableState.update {
                        it.copy(error = "Could not sign out everywhere. ${failure.toFailure().message}")
                    }
                }
            }
        }

        /** Dismiss the last error. */
        public fun clearError() {
            mutableState.update { it.copy(error = null) }
        }

        private fun edit(transform: (UserPreferences) -> UserPreferences) {
            viewModelScope.launch { preferences.update(transform) }
        }
    }
