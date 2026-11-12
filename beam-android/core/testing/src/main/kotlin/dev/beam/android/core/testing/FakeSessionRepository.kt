package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.SessionRepository
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.DeviceSession

/** Signed-in devices, held in memory. */
public class FakeSessionRepository : SessionRepository {
    /** Set to make every call throw. */
    public var failWith: BeamException? = null

    /**
     * The devices returned by [sessions].
     *
     * Populated by default, matching every other fake here: a test asserting
     * on an empty list should say so explicitly rather than depending on the
     * fake happening to start empty.
     */
    public var deviceSessions: MutableList<DeviceSession> =
        mutableListOf(Fixtures.deviceSession())

    /** Whether [logoutEverywhere] was called. */
    public var loggedOutEverywhere: Boolean = false

    override suspend fun sessions(): List<DeviceSession> {
        failWith?.let { throw it }
        return deviceSessions
    }

    override suspend fun revoke(sessionId: String) {
        failWith?.let { throw it }
        deviceSessions.removeAll { it.id == sessionId }
    }

    override suspend fun logoutEverywhere() {
        failWith?.let { throw it }
        loggedOutEverywhere = true
    }
}
