package dev.beam.android.feature.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.DeviceLoginStep
import javax.inject.Inject

/**
 * Adding a server, deciding whether to trust it, and signing in.
 *
 * Sign-in tries the device authorization grant first (ADR-0017): the screen
 * shows a code, the viewer approves it in any browser, and polling hands back
 * the session. When the server answers 501 -- its identity provider does not
 * offer the grant -- it falls back to the in-app browser, lifting the
 * `beam_session` cookie out of it, since the provider cannot redirect back to
 * a custom scheme.
 */
@HiltViewModel
public class AuthViewModel
    @Inject
    constructor(
        private val servers: ServerRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(AuthUiState())

        /** The poll loop of the device login in progress, if any. */
        private var devicePolling: Job? = null
        public val state: StateFlow<AuthUiState> = mutableState.asStateFlow()

        init {
            viewModelScope.launch {
                val known = runCatching { servers.restore() }.getOrDefault(emptyList())
                mutableState.update { it.copy(knownServers = known) }
            }
        }

        /** The viewer typed an address. */
        public fun onAddressChange(value: String) {
            mutableState.update { it.copy(address = value, error = null) }
        }

        /** Try the typed address, or an already-known server. */
        public fun connect(existingServerId: String? = null) {
            val current = mutableState.value
            if (existingServerId == null && !current.canConnect) return

            mutableState.update { it.copy(isConnecting = true, error = null, pendingTrust = null) }
            viewModelScope.launch {
                try {
                    val serverId =
                        existingServerId ?: servers
                            .addServer(current.address.trim(), displayName = null)
                            .id
                    servers.selectServer(serverId)
                    beginSignIn(serverId)
                } catch (failure: BeamException) {
                    mutableState.update { it.copy(isConnecting = false).withFailure(failure) }
                }
            }
        }

        /**
         * The viewer accepted a certificate. Retry the connection that failed.
         *
         * Retried automatically rather than making them press connect again: they
         * have already expressed the intent twice, and asking a third time is just
         * friction.
         */
        public fun acceptCertificate(trust: PendingTrust) {
            mutableState.update { it.copy(pendingTrust = null, isConnecting = true) }
            viewModelScope.launch {
                try {
                    servers.trustCertificate(trust.serverId, trust.details.sha256Fingerprint)
                    beginSignIn(trust.serverId)
                } catch (failure: BeamException) {
                    mutableState.update { it.copy(isConnecting = false).withFailure(failure) }
                }
            }
        }

        /**
         * Device login first; the in-app browser when the server says it has
         * no device grant. Any other failure is the caller's to report.
         */
        private suspend fun beginSignIn(serverId: String) {
            mutableState.update { it.copy(serverId = serverId) }
            val prompt =
                try {
                    servers.startDeviceLogin(serverId)
                } catch (unsupported: BeamException.Server) {
                    if (unsupported.status.toInt() != NOT_IMPLEMENTED) throw unsupported
                    val url = servers.loginUrl(serverId)
                    mutableState.update { it.copy(isConnecting = false, loginUrl = url) }
                    return
                }
            mutableState.update { it.copy(isConnecting = false, devicePrompt = prompt) }
            devicePolling?.cancel()
            devicePolling =
                viewModelScope.launch {
                    pollDeviceLogin(serverId, prompt.deviceHandle, prompt.intervalSecs)
                }
        }

        /** Poll until the viewer approves, refuses, or the code expires. */
        private suspend fun pollDeviceLogin(
            serverId: String,
            deviceHandle: String,
            firstIntervalSecs: UInt,
        ) {
            var intervalSecs = firstIntervalSecs
            while (true) {
                delay(intervalSecs.toLong() * MILLIS_PER_SECOND)
                val step =
                    try {
                        servers.pollDeviceLogin(serverId, deviceHandle)
                    } catch (failure: BeamException) {
                        mutableState.update { it.copy(devicePrompt = null).withFailure(failure) }
                        return
                    }
                when (step) {
                    is DeviceLoginStep.Waiting -> {
                        intervalSecs = step.intervalSecs
                    }

                    is DeviceLoginStep.SignedIn -> {
                        mutableState.update { it.copy(devicePrompt = null, isSignedIn = true) }
                        return
                    }
                }
            }
        }

        /** The viewer gave up waiting for the device login. */
        public fun onDeviceLoginCancelled() {
            devicePolling?.cancel()
            devicePolling = null
            mutableState.update { it.copy(devicePrompt = null) }
        }

        /** The viewer declined a certificate. */
        public fun declineCertificate() {
            mutableState.update {
                it.copy(
                    pendingTrust = null,
                    error = "The server's certificate was not accepted, so it was not added.",
                )
            }
        }

        /** The browser produced a session cookie. */
        public fun onSessionCookie(cookie: String) {
            val serverId = mutableState.value.serverId ?: return
            viewModelScope.launch {
                try {
                    servers.completeLogin(serverId, cookie)
                    mutableState.update { it.copy(isSignedIn = true, loginUrl = null) }
                } catch (failure: BeamException) {
                    mutableState.update { it.copy(loginUrl = null).withFailure(failure) }
                }
            }
        }

        /** The viewer closed the sign-in browser without finishing. */
        public fun onSignInCancelled() {
            mutableState.update { it.copy(loginUrl = null) }
        }

        /** Forget a server offered as a shortcut. */
        public fun forget(serverId: String) {
            viewModelScope.launch {
                runCatching { servers.removeServer(serverId) }
                val known = runCatching { servers.restore() }.getOrDefault(emptyList())
                mutableState.update { it.copy(knownServers = known) }
            }
        }

        private fun AuthUiState.withFailure(failure: BeamException): AuthUiState =
            // An untrusted certificate is a question, not an error: the viewer is
            // the only one who can answer it, and the core has already collected
            // everything they need in order to.
            if (failure is BeamException.UntrustedCertificate) {
                copy(
                    pendingTrust =
                        PendingTrust(
                            serverId = serverId ?: mutableState.value.serverId.orEmpty(),
                            host = failure.host,
                            details = failure.details,
                        ),
                    error = null,
                )
            } else {
                copy(error = failure.toFailure().message)
            }

        private companion object {
            /** The status a server without the device grant answers. */
            const val NOT_IMPLEMENTED = 501
            const val MILLIS_PER_SECOND = 1_000L
        }
    }
