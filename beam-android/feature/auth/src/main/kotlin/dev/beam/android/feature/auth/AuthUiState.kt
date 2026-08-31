package dev.beam.android.feature.auth

import uniffi.beam_client_core.CertificateDetails
import uniffi.beam_client_core.ServerSummary

/** Where the sign-in flow has got to. */
public data class AuthUiState(
    /** Servers already known, offered as shortcuts. */
    val knownServers: List<ServerSummary> = emptyList(),
    /** What the viewer has typed. */
    val address: String = "",
    /** Whether a connection attempt is in flight. */
    val isConnecting: Boolean = false,
    /** Why the last attempt failed, phrased for a person. */
    val error: String? = null,
    /**
     * The certificate to ask about, when the server presented one the platform
     * would not accept.
     */
    val pendingTrust: PendingTrust? = null,
    /** The URL to open in the sign-in browser, once a server is reachable. */
    val loginUrl: String? = null,
    /** The server being signed in to. */
    val serverId: String? = null,
    /** Set once the viewer is signed in. */
    val isSignedIn: Boolean = false,
) {
    /** Whether the address is worth trying at all. */
    public val canConnect: Boolean
        get() = address.isNotBlank() && !isConnecting
}

/** A certificate the viewer is being asked to accept. */
public data class PendingTrust(
    /** The server that presented it. */
    val serverId: String,
    /** The host it was presented for. */
    val host: String,
    /** Everything needed to make the decision an informed one. */
    val details: CertificateDetails,
)
