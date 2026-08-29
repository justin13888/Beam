package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.repository.SessionRepository
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.BrowseQuery
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.DeviceProfile
import uniffi.beam_client_core.DeviceSession
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.HistoryPage
import uniffi.beam_client_core.LibraryFileSummary
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.MediaDetail
import uniffi.beam_client_core.MediaPage
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.PlaybackHttpConfig
import uniffi.beam_client_core.ProgressOutcome
import uniffi.beam_client_core.QualityPolicy
import uniffi.beam_client_core.ServerHealth
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState
import uniffi.beam_client_core.SourceSelection
import uniffi.beam_client_core.UserSummary

// Stateful fakes rather than mocks, per the repository's testing rules: a
// screen that adds a server and then lists servers should see it, and only a
// fake that actually holds state can prove that. Each carries a `failWith`
// so the error paths the repo requires to be codified are one line to set up.

/** A catalog that answers from memory. */
public class FakeCatalogRepository : CatalogRepository {
    /** Set to make every call throw, for testing failure rendering. */
    public var failWith: BeamException? = null

    /** Pages returned by [browse], in call order; the last repeats. */
    public var pages: List<MediaPage> = listOf(Fixtures.page())

    /** Details keyed by media id. */
    public var details: MutableMap<String, MediaDetail> = mutableMapOf()

    /** Genres returned by [genres]. */
    public var genreList: List<String> = listOf("Crime", "Drama", "Thriller")

    /** Libraries returned by [libraries]. */
    public var libraryList: List<LibrarySummary> = listOf(Fixtures.library())

    /** Files returned by [libraryFiles]. */
    public var files: List<LibraryFileSummary> = emptyList()

    /** The episode [upNext] resolves to. */
    public var nextEpisode: EpisodeSummary? = null

    /** Every query [browse] was called with, in order. */
    public val browseCalls: MutableList<BrowseQuery> = mutableListOf()

    override suspend fun browse(query: BrowseQuery): MediaPage {
        failWith?.let { throw it }
        browseCalls += query
        return pages.getOrElse(browseCalls.size - 1) { pages.last() }
    }

    override suspend fun detail(mediaId: String): MediaDetail {
        failWith?.let { throw it }
        return details[mediaId]
            ?: throw BeamException.NotFound("No title with id $mediaId")
    }

    override suspend fun genres(): List<String> {
        failWith?.let { throw it }
        return genreList
    }

    override suspend fun libraries(): List<LibrarySummary> {
        failWith?.let { throw it }
        return libraryList
    }

    override suspend fun library(libraryId: String): LibrarySummary {
        failWith?.let { throw it }
        return libraryList.firstOrNull { it.id == libraryId }
            ?: throw BeamException.NotFound("No library with id $libraryId")
    }

    override suspend fun libraryFiles(libraryId: String): List<LibraryFileSummary> {
        failWith?.let { throw it }
        return files
    }

    override suspend fun upNext(showId: String, currentEpisodeId: String): EpisodeSummary? {
        failWith?.let { throw it }
        return nextEpisode
    }
}

/** A playback surface that records what it was told. */
public class FakePlaybackRepository : PlaybackRepository {
    /** Set to make every call throw. */
    public var failWith: BeamException? = null

    /** The profile last handed to [setDeviceProfile]. */
    public var deviceProfile: DeviceProfile? = null

    /** Sources returned by [sources]. */
    public var sourceList: List<MediaSourceView> = listOf(Fixtures.source())

    /** The selection returned by [selectSource]. */
    public var selection: SourceSelection? = null

    /** Rows returned by [continueWatching]. */
    public var continueWatchingRows: List<ContinueWatchingEntry> =
        listOf(Fixtures.continueWatching())

    /** The page returned by [history]. */
    public var historyPage: HistoryPage = HistoryPage(listOf(Fixtures.historyEntry()), 1uL)

    /** Every progress report, in order, as (fileId, position, force). */
    public val progressReports: MutableList<Triple<String, Double, Boolean>> = mutableListOf()

    /** What [reportProgress] returns. */
    public var progressOutcome: ProgressOutcome = ProgressOutcome.Sent(0.0)

    /** How many entries [flushProgress] claims to have sent. */
    public var flushed: UInt = 0u

    /** How many entries are waiting. */
    public var pending: UInt = 0u

    override suspend fun setDeviceProfile(profile: DeviceProfile) {
        deviceProfile = profile
    }

    override suspend fun sources(mediaId: String): List<MediaSourceView> {
        failWith?.let { throw it }
        return sourceList
    }

    override suspend fun selectSource(mediaId: String, policy: QualityPolicy): SourceSelection {
        failWith?.let { throw it }
        return selection ?: throw BeamException.NotFound("This title has no playable files")
    }

    override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig {
        failWith?.let { throw it }
        return PlaybackHttpConfig(
            url = "https://beam.test/v1/files/$fileId/stream",
            headers = mapOf("Cookie" to "beam_session=test"),
            certificatePins = emptyList(),
            pinnedHost = "beam.test",
        )
    }

    override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> {
        failWith?.let { throw it }
        return continueWatchingRows
    }

    override suspend fun history(limit: UInt?, offset: UInt?): HistoryPage {
        failWith?.let { throw it }
        return historyPage
    }

    override suspend fun reportProgress(
        fileId: String,
        positionSecs: Double,
        durationSecs: Double?,
        force: Boolean,
    ): ProgressOutcome {
        progressReports += Triple(fileId, positionSecs, force)
        failWith?.let { throw it }
        return progressOutcome
    }

    override suspend fun flushProgress(): UInt {
        failWith?.let { throw it }
        return flushed
    }

    override suspend fun pendingProgressCount(): UInt = pending
}

/** A server registry that actually holds servers. */
public class FakeServerRepository(
    initial: List<ServerSummary> = listOf(Fixtures.server()),
) : ServerRepository {
    /** Set to make every call throw. */
    public var failWith: BeamException? = null

    private val state = MutableStateFlow(initial)

    override val servers: Flow<List<ServerSummary>> = state

    /** The cookie last handed to [completeLogin]. */
    public var capturedCookie: String? = null

    override suspend fun restore(): List<ServerSummary> {
        failWith?.let { throw it }
        return state.value
    }

    override suspend fun addServer(baseUrl: String, displayName: String?): ServerSummary {
        failWith?.let { throw it }
        val added = Fixtures.server(
            id = baseUrl.filter { it.isLetterOrDigit() },
            displayName = displayName ?: baseUrl,
            state = SessionState.LoggedOut,
        )
        state.value = state.value.map { it.copy(isActive = false) } + added
        return added
    }

    override suspend fun selectServer(serverId: String) {
        failWith?.let { throw it }
        state.value = state.value.map { it.copy(isActive = it.id == serverId) }
    }

    override suspend fun removeServer(serverId: String) {
        failWith?.let { throw it }
        state.value = state.value.filterNot { it.id == serverId }
    }

    override suspend fun loginUrl(serverId: String): String =
        "https://beam.test/v1/auth/login?redirect=/"

    override suspend fun completeLogin(serverId: String, sessionCookie: String): UserSummary {
        capturedCookie = sessionCookie
        failWith?.let { throw it }
        val user = Fixtures.user()
        state.value = state.value.map {
            if (it.id == serverId) it.copy(state = SessionState.Authenticated(user)) else it
        }
        return user
    }

    override suspend fun logout(serverId: String) {
        state.value = state.value.map {
            if (it.id == serverId) it.copy(state = SessionState.LoggedOut) else it
        }
    }

    override suspend fun sessionState(serverId: String): SessionState =
        state.value.firstOrNull { it.id == serverId }?.state ?: SessionState.LoggedOut

    override suspend fun activeServer(): ServerSummary? = state.value.firstOrNull { it.isActive }
}

/** Signed-in devices, held in memory. */
public class FakeSessionRepository : SessionRepository {
    /** Set to make every call throw. */
    public var failWith: BeamException? = null

    /** The devices returned by [sessions]. */
    public var deviceSessions: MutableList<DeviceSession> = mutableListOf()

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

/** An administrative surface that answers from memory. */
public class FakeAdminRepository : AdminRepository {
    /** Set to make every call throw, typically `Forbidden`. */
    public var failWith: BeamException? = null

    /** The dashboard snapshot. */
    public var statusValue: AdminStatus = Fixtures.adminStatus()

    /** Accounts returned by [users]. */
    public var userPage: AdminUserPage = AdminUserPage(emptyList(), 0uL)

    /** Log lines returned by [logs]. */
    public var logEntries: List<AdminLogEntry> = emptyList()

    /** Events returned by [events]. */
    public var eventEntries: List<AdminEvent> = emptyList()

    /** Every (userId, disabled) pair passed to [setUserDisabled]. */
    public val disableCalls: MutableList<Pair<String, Boolean>> = mutableListOf()

    /** Every library id passed to [scanLibrary]. */
    public val scanCalls: MutableList<String> = mutableListOf()

    override suspend fun status(): AdminStatus {
        failWith?.let { throw it }
        return statusValue
    }

    override suspend fun health(): ServerHealth {
        failWith?.let { throw it }
        return ServerHealth("ok", "0.1.0", 86_400uL, "ok")
    }

    override suspend fun users(limit: UInt?, offset: UInt?): AdminUserPage {
        failWith?.let { throw it }
        return userPage
    }

    override suspend fun setUserDisabled(userId: String, disabled: Boolean) {
        failWith?.let { throw it }
        disableCalls += userId to disabled
    }

    override suspend fun logs(limit: UInt?, offset: UInt?): List<AdminLogEntry> {
        failWith?.let { throw it }
        return logEntries
    }

    override suspend fun logCount(): ULong {
        failWith?.let { throw it }
        return logEntries.size.toULong()
    }

    override suspend fun events(limit: UInt?): List<AdminEvent> {
        failWith?.let { throw it }
        return eventEntries
    }

    override suspend fun createLibrary(name: String, rootPath: String): LibrarySummary {
        failWith?.let { throw it }
        return Fixtures.library(name = name)
    }

    override suspend fun deleteLibrary(libraryId: String) {
        failWith?.let { throw it }
    }

    override suspend fun scanLibrary(libraryId: String): UInt {
        failWith?.let { throw it }
        scanCalls += libraryId
        return 7u
    }

    override suspend fun refreshMetadata(mediaId: String) {
        failWith?.let { throw it }
    }
}
