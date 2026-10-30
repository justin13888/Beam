package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.ServerRepository
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.DeviceLoginPrompt
import uniffi.beam_client_core.DeviceLoginStep
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState
import uniffi.beam_client_core.UserSummary
import java.net.URI

// Stateful fakes rather than mocks, per the repository's testing rules: a
// screen that adds a server and then lists servers should see it, and only a
// fake that actually holds state can prove that. Each fake in this package
// carries a `failWith` so the error paths the repo requires to be codified are one line to set up.

/**
 * A server registry that actually holds servers, and behaves as the real
 * repository over the core does.
 *
 * That means three things a simpler fake got wrong. The core starts with no
 * servers: [stored] is what a previous run left on disk, and nothing is
 * listed until [restore] reads it. [servers] re-publishes on exactly the
 * calls `BeamServerRepository` re-publishes on, so a test of a screen that
 * watches it sees what a phone would. And the calls the core answers locally
 * -- adding, selecting, the sign-in address -- never fail the way a network
 * call does; [failOnce] reaches only the calls that go to the server.
 */
public class FakeServerRepository(
    stored: List<ServerSummary> = emptyList(),
    restored: Boolean = false,
) : ServerRepository {
    /** Set to make every call throw. */
    public var failWith: BeamException? = null

    /** The core's own registry: what `listServers` would read now. */
    private var registry: List<ServerSummary> = sorted(stored)

    /**
     * What the repository re-publishes. A replaying shared flow, as the real
     * `ServerChanges` is: it replays nothing before [restore], and it emits
     * every re-publication, an unchanged list included -- which a state flow
     * would drop. Unbounded rather than suspending, so [expire] can publish
     * without a coroutine; every subscriber still sees every list.
     */
    private val published =
        MutableSharedFlow<List<ServerSummary>>(replay = 1, extraBufferCapacity = Int.MAX_VALUE).apply {
            if (restored) tryEmit(registry)
        }

    override val servers: Flow<List<ServerSummary>> = published.asSharedFlow()

    /** The cookie last handed to [completeLogin]. */
    public var capturedCookie: String? = null

    /** Every server [selectServer] was asked for, in order. */
    public val selectCalls: MutableList<String> = mutableListOf()

    override suspend fun restore(): List<ServerSummary> {
        failWith?.let { throw it }
        return registry.also { published.tryEmit(it) }
    }

    /**
     * Idempotent, as the core is: the address is normalised to an origin and
     * the server named for it, so the same server typed twice -- or once with
     * a trailing slash -- is one entry, selected.
     */
    override suspend fun addServer(
        baseUrl: String,
        displayName: String?,
    ): ServerSummary {
        failWith?.let { throw it }
        val origin = originOf(baseUrl)
        val id = idFor(origin)
        if (registry.none { it.id == id }) {
            registry =
                sorted(
                    registry +
                        ServerSummary(
                            id = id,
                            displayName = displayName ?: origin.host,
                            baseUrl = origin.toString(),
                            state = SessionState.LoggedOut,
                            isActive = false,
                        ),
                )
        }
        activate(id)
        republish()
        return registry.single { it.id == id }
    }

    override suspend fun selectServer(serverId: String) {
        selectCalls += serverId
        failWith?.let { throw it }
        if (registry.none { it.id == serverId }) throw BeamException.UnknownServer(serverId)
        activate(serverId)
        republish()
    }

    override suspend fun removeServer(serverId: String) {
        failWith?.let { throw it }
        registry = registry.filterNot { it.id == serverId }
        republish()
    }

    /** Built locally by the core, which can fail only for a server it does not know. */
    override suspend fun loginUrl(serverId: String): String {
        failWith?.let { throw it }
        val server = registry.firstOrNull { it.id == serverId } ?: throw BeamException.UnknownServer(serverId)
        return "${server.baseUrl}/v1/auth/login?redirect=/"
    }

    /**
     * What successive device-login polls answer, front first; the last one
     * repeats. `null` models an identity provider without the device grant:
     * starting answers the server's 501.
     */
    public var deviceLogin: List<DeviceLoginStep>? = null

    /** How many times a device login was polled, failed polls included. */
    public var devicePolls: Int = 0

    /**
     * Failures the next polls throw, front first, before the [deviceLogin]
     * script resumes. Each one counts as a poll.
     */
    public val devicePollFailures: ArrayDeque<BeamException> = ArrayDeque()

    /** How many polls the [deviceLogin] script has answered. */
    private var scriptedPolls: Int = 0

    override suspend fun startDeviceLogin(serverId: String): DeviceLoginPrompt {
        failOnce?.let {
            failOnce = null
            throw it
        }
        failWith?.let { throw it }
        if (deviceLogin == null) {
            throw BeamException.Server(
                501u,
                false,
                "no device grant",
                "https://beam.justinchung.net/reference/errors/#device-login-unsupported",
            )
        }
        return DeviceLoginPrompt(
            deviceHandle = "handle-1",
            userCode = "BCDF-GHJK",
            verificationUri = "https://idp.test/device",
            verificationUriComplete = null,
            expiresInSecs = 600uL,
            intervalSecs = 5u,
        )
    }

    override suspend fun pollDeviceLogin(
        serverId: String,
        deviceHandle: String,
    ): DeviceLoginStep {
        failWith?.let { throw it }
        val script = checkNotNull(deviceLogin) { "no device login was started" }
        devicePollFailures.removeFirstOrNull()?.let {
            devicePolls += 1
            throw it
        }
        val step = script[minOf(scriptedPolls, script.lastIndex)]
        scriptedPolls += 1
        devicePolls += 1
        if (step is DeviceLoginStep.SignedIn) {
            setState(serverId, SessionState.Authenticated(step.user))
            republish()
        }
        return step
    }

    /**
     * Confirms the cookie with the server. A refusal of any kind withdraws
     * it and leaves the server signed out, as the core's `IdentityRejected`
     * does -- and, as the real repository does, re-publishes only on success.
     */
    override suspend fun completeLogin(
        serverId: String,
        sessionCookie: String,
    ): UserSummary {
        capturedCookie = sessionCookie
        val failure =
            failOnce?.also { failOnce = null }
                ?: failWith
        if (failure != null) {
            // Whatever the failure, an untrusted certificate included: the
            // core's `adopt_session` applies `IdentityRejected` on any error
            // from `/v1/me`.
            setState(serverId, SessionState.LoggedOut)
            throw failure
        }
        val user = Fixtures.user()
        setState(serverId, SessionState.Authenticated(user))
        republish()
        return user
    }

    /**
     * Signed out on this device whatever the server says, as the core is; it
     * fails only when the stored cookie cannot be cleared, which is what
     * [failWith] stands for here.
     */
    override suspend fun logout(serverId: String) {
        failWith?.let { throw it }
        setState(serverId, SessionState.LoggedOut)
        republish()
    }

    /**
     * The server refusing [serverId]'s session: what the core's state reads
     * after any secured call is answered `401`, re-published as the real
     * repository re-publishes it.
     */
    public fun expire(serverId: String) {
        setState(serverId, SessionState.Expired)
        republish()
    }

    /** Fingerprints accepted, per server. */
    public val trusted: MutableMap<String, MutableList<String>> = mutableMapOf()

    /**
     * Set to make the next call that reaches the server -- starting a device
     * login or confirming a cookie -- throw once, then clear itself.
     *
     * Distinct from [failWith], which fails everything: a trust prompt is
     * driven by a failure that must *stop* failing once the certificate is
     * accepted, and a permanent failure cannot express that. Not consumed by
     * the calls the core answers locally, which cannot see a certificate.
     */
    public var failOnce: BeamException? = null

    override suspend fun trustCertificate(
        serverId: String,
        fingerprint: String,
    ) {
        failWith?.let { throw it }
        trusted.getOrPut(serverId) { mutableListOf() } += fingerprint
    }

    override suspend fun forgetCertificates(serverId: String) {
        failWith?.let { throw it }
        trusted.remove(serverId)
        republish()
    }

    override suspend fun trustedCertificates(serverId: String): List<String> {
        failWith?.let { throw it }
        return trusted[serverId].orEmpty()
    }

    override suspend fun activeServer(): ServerSummary? = registry.firstOrNull { it.isActive }

    private fun republish() {
        published.tryEmit(registry)
    }

    private fun activate(serverId: String) {
        registry = registry.map { it.copy(isActive = it.id == serverId) }
    }

    private fun setState(
        serverId: String,
        state: SessionState,
    ) {
        registry = registry.map { if (it.id == serverId) it.copy(state = state) else it }
    }

    private companion object {
        /** The core lists servers by display name. */
        fun sorted(servers: List<ServerSummary>): List<ServerSummary> = servers.sortedBy { it.displayName }

        /** `beam.local`, `https://beam.local/` and `HTTPS://Beam.Local` are one origin, defaulting to HTTPS. */
        fun originOf(address: String): URI {
            val trimmed = address.trim()
            val parsed = URI(if ("://" in trimmed) trimmed else "https://$trimmed")
            return URI(parsed.scheme.lowercase(), null, parsed.host.lowercase(), parsed.port, null, null, null)
        }

        /** The core's `server_id_for`: scheme, host and any port, joined by dashes. */
        fun idFor(origin: URI): String =
            listOfNotNull(origin.scheme, origin.host, origin.port.takeIf { it >= 0 }?.toString())
                .joinToString("-")
                .replace(Regex("[.:/]"), "-")
    }
}
