package dev.beam.android

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.DeviceProfiles
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.model.UserPreferences
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.SessionState
import javax.inject.Inject

/** What the shell needs before it can render anything. */
public data class MainUiState(
    /** Whether the stored session has been read yet. */
    val isReady: Boolean = false,
    /**
     * Whether there is a usable session.
     *
     * A stored session counts from the moment it is restored -- the core
     * restores it signed in without asking the server -- and stops counting
     * when the server refuses it.
     */
    val isSignedIn: Boolean = false,
    /**
     * Whether the server has refused the active server's session, which is
     * the shell's cue to send the viewer back to sign-in wherever they are.
     */
    val isSessionExpired: Boolean = false,
    /** Whether the signed-in user administers this server. */
    val isAdmin: Boolean = false,
    /** The current preferences, which the theme reads. */
    val preferences: UserPreferences = UserPreferences(),
)

/** Restores the session and tells the core what this device can decode. */
@HiltViewModel
public class MainViewModel
    @Inject
    constructor(
        private val servers: ServerRepository,
        private val playback: PlaybackRepository,
        private val preferences: PreferencesRepository,
        @dagger.hilt.android.qualifiers.ApplicationContext private val context: android.content.Context,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(MainUiState())
        public val state: StateFlow<MainUiState> = mutableState.asStateFlow()

        init {
            viewModelScope.launch {
                preferences.preferences.collect { value ->
                    mutableState.update { it.copy(preferences = value) }
                }
            }
            viewModelScope.launch {
                val allowSoftware = preferences.preferences.first().allowSoftwareDecode
                // The capability profile is sent before anything is browsed, so the
                // first "play" the viewer taps already knows what this device can
                // decode. Sending it lazily would mean the first selection ran
                // against an empty profile and rejected every file.
                runCatching { playback.setDeviceProfile(DeviceProfiles.build(context, allowSoftware)) }

                val active =
                    runCatching { servers.restore() }
                        .getOrDefault(emptyList())
                        .firstOrNull { it.isActive }
                val session = active?.state

                mutableState.update {
                    it.copy(
                        isReady = true,
                        isSignedIn = session is SessionState.Authenticated,
                        isSessionExpired = session is SessionState.Expired,
                        isAdmin = (session as? SessionState.Authenticated)?.user?.isAdmin ?: false,
                    )
                }
            }
            viewModelScope.launch {
                // Who administers what is per server: switching servers in
                // settings, or signing in to another, changes it. So does a
                // session the server refuses mid-use.
                servers.servers.collect { known ->
                    val session = known.firstOrNull { it.isActive }?.state
                    mutableState.update {
                        it.copy(
                            // Before the restore has answered, the list is not
                            // yet the one this device restored.
                            isSignedIn = if (it.isReady) session is SessionState.Authenticated else it.isSignedIn,
                            isSessionExpired = session is SessionState.Expired,
                            isAdmin = (session as? SessionState.Authenticated)?.user?.isAdmin ?: false,
                        )
                    }
                }
            }
        }
    }
