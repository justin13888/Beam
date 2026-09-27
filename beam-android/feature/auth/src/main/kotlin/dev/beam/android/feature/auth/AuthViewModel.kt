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
import uniffi.beam_client_core.DeviceLoginPrompt
import uniffi.beam_client_core.DeviceLoginStep
import javax.inject.Inject

/**
 * Adding a server, deciding whether to trust it, and signing in.
 *
 * Where there is a usable browser -- a phone -- sign-in opens the in-app
 * browser, lifting the `beam_session` cookie out of it since the provider
 * cannot redirect back to a custom scheme, and [signInWithCode] offers the
 * device authorization grant (ADR-0017) as a second choice. Where there is
 * none -- a TV -- the device grant comes first: the screen shows a code, the
 * viewer approves it in any browser, and polling hands back the session; the
 * in-app browser is the fallback when the server answers 501 because its
 * identity provider does not offer the grant.
 */
@HiltViewModel
public class AuthViewModel
    @Inject
    constructor(
        private val servers: ServerRepository,
        private val browser: BrowserAvailability,
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
         * The in-app browser where there is one. Otherwise device login
         * first, and the in-app browser when the server says it has no device
         * grant. Any other failure is the caller's to report.
         */
        private suspend fun beginSignIn(serverId: String) {
            mutableState.update { it.copy(serverId = serverId) }
            if (browser.hasUsableBrowser()) {
                val url = servers.loginUrl(serverId)
                mutableState.update { it.copy(isConnecting = false, loginUrl = url) }
                return
            }
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
            startPolling(serverId, prompt)
        }

        /**
         * The viewer chose to sign in with a code rather than in the in-app
         * browser. The browser stays open until the server has issued one, so
         * a server without the device grant leaves the viewer where they were.
         */
        public fun signInWithCode() {
            val serverId = mutableState.value.serverId ?: return
            mutableState.update { it.copy(error = null) }
            viewModelScope.launch {
                val prompt =
                    try {
                        servers.startDeviceLogin(serverId)
                    } catch (failure: BeamException) {
                        mutableState.update { it.withFailure(failure) }
                        return@launch
                    }
                mutableState.update { it.copy(loginUrl = null, devicePrompt = prompt) }
                startPolling(serverId, prompt)
            }
        }

        private fun startPolling(
            serverId: String,
            prompt: DeviceLoginPrompt,
        ) {
            devicePolling?.cancel()
            devicePolling =
                viewModelScope.launch {
                    pollDeviceLogin(serverId, prompt.deviceHandle, prompt.intervalSecs)
                }
        }

        /**
         * Poll until the viewer approves, refuses, or the code expires.
         *
         * A failure that a later poll could get past -- a retryable server
         * error, a rate limit, a lost connection -- does not end the login:
         * the viewer may be halfway through approving it on their phone. The
         * next poll waits the current interval, or as long as a rate limit
         * asks if that is longer. Anything else (a refusal, an expiry, a flow
         * the server no longer knows) ends it with the failure shown.
         */
        private suspend fun pollDeviceLogin(
            serverId: String,
            deviceHandle: String,
            firstIntervalSecs: UInt,
        ) {
            var intervalSecs = firstIntervalSecs.toLong()
            var waitSecs = intervalSecs
            while (true) {
                delay(waitSecs * MILLIS_PER_SECOND)
                waitSecs = intervalSecs
                val step =
                    try {
                        servers.pollDeviceLogin(serverId, deviceHandle)
                    } catch (failure: BeamException) {
                        if (failure is BeamException.RateLimited) {
                            waitSecs = maxOf(intervalSecs, failure.retryAfterSecs.toLong())
                            continue
                        }
                        if (failure.keepsDeviceLoginAlive()) continue
                        mutableState.update { it.copy(devicePrompt = null).withFailure(failure) }
                        return
                    }
                when (step) {
                    is DeviceLoginStep.Waiting -> {
                        intervalSecs = step.intervalSecs.toLong()
                        waitSecs = intervalSecs
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

        /** Whether a poll that failed this way is worth repeating. */
        private fun BeamException.keepsDeviceLoginAlive(): Boolean =
            when (this) {
                is BeamException.Server -> retryable
                is BeamException.Network -> retryable
                else -> false
            }

        private companion object {
            /** The status a server without the device grant answers. */
            const val NOT_IMPLEMENTED = 501
            const val MILLIS_PER_SECOND = 1_000L
        }
    }
