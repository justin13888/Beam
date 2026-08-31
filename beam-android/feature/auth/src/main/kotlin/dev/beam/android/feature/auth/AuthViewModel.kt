package dev.beam.android.feature.auth

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.BeamException
import javax.inject.Inject

/**
 * Adding a server, deciding whether to trust it, and signing in.
 *
 * Sign-in is a browser flow rather than a native form, and not by preference:
 * `beam-server` reads exactly one credential, the `beam_session` cookie, and
 * `sanitize_redirect_path` accepts only same-origin relative paths -- so the
 * OIDC provider cannot redirect back to a custom scheme. Lifting the cookie
 * out of the in-app browser is the only flow the server actually supports.
 */
@HiltViewModel
public class AuthViewModel
    @Inject
    constructor(
        private val servers: ServerRepository,
    ) : ViewModel() {
        private val mutableState = MutableStateFlow(AuthUiState())
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
                    val url = servers.loginUrl(serverId)
                    mutableState.update {
                        it.copy(isConnecting = false, loginUrl = url, serverId = serverId)
                    }
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
                    val url = servers.loginUrl(trust.serverId)
                    mutableState.update {
                        it.copy(isConnecting = false, loginUrl = url, serverId = trust.serverId)
                    }
                } catch (failure: BeamException) {
                    mutableState.update { it.copy(isConnecting = false).withFailure(failure) }
                }
            }
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
    }
