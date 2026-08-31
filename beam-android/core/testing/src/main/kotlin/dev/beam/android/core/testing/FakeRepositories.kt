package dev.beam.android.core.testing

import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.repository.SessionRepository
import dev.beam.android.core.model.UserPreferences
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUser
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

    /**
     * The cursor every page reports, or null for "this is the last page".
     *
     * Overrides whatever [pages] carries, so a paging test can be written
     * without constructing a page fixture per page.
     */
    public var nextCursor: String? = null

    /**
     * Set to make [genres] alone fail.
     *
     * Distinct from [failWith] because the genre chips and the catalog grid
     * are separate concerns: one failing must not be indistinguishable from
     * both failing.
     */
    public var genresFailWith: BeamException? = null

    override suspend fun browse(query: BrowseQuery): MediaPage {
        failWith?.let { throw it }
        browseCalls += query
        val page = pages.getOrElse(browseCalls.size - 1) { pages.last() }
        return page.copy(endCursor = nextCursor, hasNextPage = nextCursor != null)
    }

    override suspend fun detail(mediaId: String): MediaDetail {
        failWith?.let { throw it }
        return details[mediaId]
            ?: throw BeamException.NotFound("No title with id $mediaId")
    }

    override suspend fun genres(): List<String> {
        genresFailWith?.let { throw it }
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

    override suspend fun upNext(
        showId: String,
        currentEpisodeId: String,
    ): EpisodeSummary? {
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

    /**
     * Every progress report, in order.
     *
     * Records the duration and the force flag as well as the position, because
     * the interesting assertions are about *those*: whether a pause forced a
     * send, and whether an unknown duration was sent as absent rather than as
     * a confident zero.
     */
    public val reportedProgress: MutableList<ProgressReport> = mutableListOf()

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

    /** The policy [selectSource] was last asked for. */
    public var selectionPolicy: QualityPolicy? = null
        private set

    override suspend fun selectSource(
        mediaId: String,
        policy: QualityPolicy,
    ): SourceSelection {
        selectionPolicy = policy
        failWith?.let { throw it }
        return selection ?: throw BeamException.NotFound("This title has no playable files")
    }

    override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig {
        failWith?.let { throw it }
        return PlaybackHttpConfig(
            url = "https://beam.test/v1/files/$fileId/stream",
            headers = mapOf("Cookie" to "beam_session=test"),
            trustedFingerprints = emptyList(),
            pinnedHost = "beam.test",
        )
    }

    override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> {
        failWith?.let { throw it }
        return continueWatchingRows
    }

    override suspend fun history(
        limit: UInt?,
        offset: UInt?,
    ): HistoryPage {
        failWith?.let { throw it }
        return historyPage
    }

    override suspend fun reportProgress(
        fileId: String,
        positionSecs: Double,
        durationSecs: Double?,
        force: Boolean,
    ): ProgressOutcome {
        reportedProgress += ProgressReport(fileId, positionSecs, durationSecs, force)
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

    override suspend fun addServer(
        baseUrl: String,
        displayName: String?,
    ): ServerSummary {
        failWith?.let { throw it }
        val added =
            Fixtures.server(
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

    override suspend fun loginUrl(serverId: String): String {
        failOnce?.let {
            failOnce = null
            throw it
        }
        failWith?.let { throw it }
        return "https://beam.test/v1/auth/login?redirect=/"
    }

    override suspend fun completeLogin(
        serverId: String,
        sessionCookie: String,
    ): UserSummary {
        capturedCookie = sessionCookie
        failWith?.let { throw it }
        val user = Fixtures.user()
        state.value =
            state.value.map {
                if (it.id == serverId) it.copy(state = SessionState.Authenticated(user)) else it
            }
        return user
    }

    override suspend fun logout(serverId: String) {
        state.value =
            state.value.map {
                if (it.id == serverId) it.copy(state = SessionState.LoggedOut) else it
            }
    }

    /** Fingerprints accepted, per server. */
    public val trusted: MutableMap<String, MutableList<String>> = mutableMapOf()

    /**
     * Set to make the next call throw once, then clear itself.
     *
     * Distinct from [failWith], which fails everything: a trust prompt is
     * driven by a failure that must *stop* failing once the certificate is
     * accepted, and a permanent failure cannot express that.
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
    }

    override suspend fun trustedCertificates(serverId: String): List<String> {
        failWith?.let { throw it }
        return trusted[serverId].orEmpty()
    }

    override suspend fun sessionState(serverId: String): SessionState =
        state.value.firstOrNull { it.id == serverId }?.state ?: SessionState.LoggedOut

    override suspend fun activeServer(): ServerSummary? = state.value.firstOrNull { it.isActive }
}

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

/** An administrative surface that answers from memory. */
public class FakeAdminRepository : AdminRepository {
    /** Set to make every call throw, typically `Forbidden`. */
    public var failWith: BeamException? = null

    /** The dashboard snapshot. */
    public var statusValue: AdminStatus = Fixtures.adminStatus()

    /**
     * Accounts, held as mutable state rather than a fixed page.
     *
     * Stateful because the interesting assertion is that blocking an account
     * is *reflected* afterwards -- a fake that only records the call cannot
     * tell a working implementation from one that drops the write.
     */
    public var userList: MutableList<AdminUser> =
        mutableListOf(
            Fixtures.adminUser(),
            Fixtures.adminUser(id = "user-2", displayName = "Grace Hopper", isAdmin = true),
        )

    /**
     * Set to make [users] alone fail.
     *
     * Distinct from [failWith] because the user list is one section of the
     * dashboard: it failing must be distinguishable from the whole screen
     * failing.
     */
    public var usersFailWith: BeamException? = null

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

    override suspend fun users(
        limit: UInt?,
        offset: UInt?,
    ): AdminUserPage {
        usersFailWith?.let { throw it }
        failWith?.let { throw it }
        return AdminUserPage(userList, userList.size.toULong())
    }

    override suspend fun setUserDisabled(
        userId: String,
        disabled: Boolean,
    ) {
        failWith?.let { throw it }
        disableCalls += userId to disabled
        val index = userList.indexOfFirst { it.id == userId }
        if (index >= 0) {
            userList[index] = userList[index].copy(disabled = disabled)
        }
    }

    override suspend fun logs(
        limit: UInt?,
        offset: UInt?,
    ): List<AdminLogEntry> {
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

    override suspend fun createLibrary(
        name: String,
        rootPath: String,
    ): LibrarySummary {
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

/** One position report captured by [FakePlaybackRepository]. */
public data class ProgressReport(
    /** The file the position belongs to. */
    val fileId: String,
    /** Where the viewer had got to, in seconds. */
    val positionSecs: Double,
    /** The title's length, where the player knew it. */
    val durationSecs: Double?,
    /** Whether the core was asked to bypass its throttle. */
    val force: Boolean,
)

/**
 * Preferences held in memory.
 *
 * A real [dev.beam.android.core.ffi.preferences.PreferencesRepository] is
 * DataStore-backed, which means a file, a coroutine scope and a real
 * filesystem -- none of which a view-model test should need.
 */
public class FakePreferencesRepository(
    initial: UserPreferences = UserPreferences(),
) : PreferencesRepository {
    private val state = MutableStateFlow(initial)

    override val preferences: Flow<UserPreferences> = state

    /** The current value, for assertions. */
    public val current: UserPreferences get() = state.value

    /** How many times [update] was called. */
    public var updateCount: Int = 0
        private set

    override suspend fun update(transform: (UserPreferences) -> UserPreferences) {
        updateCount++
        state.value = transform(state.value)
    }
}
