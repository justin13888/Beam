package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.SessionRepository
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.DeviceSession
import uniffi.beam_client_core.LanguagePreferences
import uniffi.beam_client_core.SubtitleMode

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

    /** The stored language preferences, which [setLanguagePreferences] replaces. */
    public var languages: LanguagePreferences =
        LanguagePreferences(
            audioLanguages = listOf("eng"),
            subtitleLanguages = emptyList(),
            subtitleMode = SubtitleMode.FORCED_ONLY,
        )

    /** Set to make only [setLanguagePreferences] throw. */
    public var failLanguageWritesWith: BeamException? = null

    override suspend fun languagePreferences(): LanguagePreferences {
        failWith?.let { throw it }
        return languages
    }

    /**
     * Stores [preferences] as [stored] answers them; by default as sent. The
     * server's normalisation -- `ja` answered as `jpn` -- is the server's, so
     * a test that relies on it says so through [stored].
     */
    override suspend fun setLanguagePreferences(preferences: LanguagePreferences): LanguagePreferences {
        failWith?.let { throw it }
        failLanguageWritesWith?.let { throw it }
        languages = stored(preferences)
        return languages
    }

    /** How [setLanguagePreferences] stores what it is sent. */
    public var stored: (LanguagePreferences) -> LanguagePreferences = { it }
}
