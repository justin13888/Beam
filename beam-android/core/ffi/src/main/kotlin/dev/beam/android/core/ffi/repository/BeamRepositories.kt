package dev.beam.android.core.ffi.repository

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogPage
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.BrowseQuery
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.DeviceLoginPrompt
import uniffi.beam_client_core.DeviceLoginStep
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
import uniffi.beam_client_core.PlaybackPlan
import uniffi.beam_client_core.PlaybackTelemetrySummary
import uniffi.beam_client_core.ProgressDrain
import uniffi.beam_client_core.ProgressOutcome
import uniffi.beam_client_core.QualityPolicy
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ServerHealth
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionObserver
import uniffi.beam_client_core.SourceSelection
import uniffi.beam_client_core.StartFailureCause
import uniffi.beam_client_core.SwitchTrigger
import uniffi.beam_client_core.TelemetryClientKind
import uniffi.beam_client_core.TelemetryFlush
import uniffi.beam_client_core.UserSummary
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The server list as the shell observes it: whatever the core announces.
 *
 * The core tells its [SessionObserver] whenever the list changes -- a
 * sign-in completing, a sign-out, a `401` expiring a session on any call, a
 * server added, removed or selected -- so no repository catches failures or
 * re-reads the list to find out. Registered here, once, because the core
 * holds one observer and this is the one place the list is published from.
 */
@Singleton
internal class ServerChanges
    @Inject
    constructor(
        client: BeamClient,
    ) {
        /**
         * Never suspends an announcement: the core calls the observer
         * synchronously, while it holds the lock that keeps announcements in
         * order, so `tryEmit` must always succeed. Unbounded rather than
         * dropping, so every collector still sees every list; announcements
         * are a handful of servers, and only as frequent as sign-ins.
         */
        private val changes =
            MutableSharedFlow<List<ServerSummary>>(replay = 1, extraBufferCapacity = Int.MAX_VALUE)

        val servers: Flow<List<ServerSummary>> = changes.asSharedFlow()

        init {
            client.setSessionObserver(
                object : SessionObserver {
                    override fun onSessionsChanged(servers: List<ServerSummary>) {
                        changes.tryEmit(servers)
                    }
                },
            )
        }
    }

/**
 * The server registry, over the core.
 *
 * The core announces the whole list after any change, through [ServerChanges].
 * A media app has a handful of servers at most, which makes a diffing protocol
 * across the FFI boundary more machinery than the problem deserves.
 */
@Singleton
internal class BeamServerRepository
    @Inject
    constructor(
        private val client: BeamClient,
        changes: ServerChanges,
    ) : ServerRepository {
        override val servers: Flow<List<ServerSummary>> = changes.servers

        override suspend fun restore(): List<ServerSummary> = client.restore()

        override suspend fun addServer(
            baseUrl: String,
            displayName: String?,
        ): ServerSummary = client.addServer(baseUrl, displayName)

        override suspend fun selectServer(serverId: String) = client.selectServer(serverId)

        override suspend fun removeServer(serverId: String) = client.removeServer(serverId)

        override suspend fun loginUrl(serverId: String): String = client.loginUrl(serverId)

        override suspend fun startDeviceLogin(serverId: String): DeviceLoginPrompt = client.startDeviceLogin(serverId)

        override suspend fun pollDeviceLogin(
            serverId: String,
            deviceHandle: String,
        ): DeviceLoginStep = client.pollDeviceLogin(serverId, deviceHandle)

        override suspend fun completeLogin(
            serverId: String,
            sessionCookie: String,
        ): UserSummary = client.completeLogin(serverId, sessionCookie)

        override suspend fun logout(serverId: String) = client.logout(serverId)

        override suspend fun trustCertificate(
            serverId: String,
            fingerprint: String,
        ) {
            client.trustCertificate(serverId, fingerprint)
        }

        // Forgetting a certificate keeps the session (the core carries it
        // across the rebuilt client), so the list is unchanged and nothing is
        // announced.
        override suspend fun forgetCertificates(serverId: String) = client.forgetCertificates(serverId)

        override suspend fun trustedCertificates(serverId: String): List<String> = client.trustedCertificates(serverId)

        override suspend fun activeServer(): ServerSummary? = client.listServers().firstOrNull { it.isActive }
    }

@Singleton
internal class BeamCatalogRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : CatalogRepository {
        override suspend fun browse(query: BrowseQuery): MediaPage = client.browseMedia(query)

        override suspend fun detail(mediaId: String): MediaDetail = client.mediaDetail(mediaId)

        override suspend fun genres(): List<String> = client.genres()

        override suspend fun libraries(): List<LibrarySummary> = client.libraries()

        override suspend fun library(libraryId: String): LibrarySummary = client.library(libraryId)

        override suspend fun libraryFiles(libraryId: String): List<LibraryFileSummary> = client.libraryFiles(libraryId)

        override suspend fun upNext(
            showId: String,
            currentEpisodeId: String,
        ): EpisodeSummary? = client.upNextInShow(showId, currentEpisodeId)
    }

@Singleton
internal class BeamPlaybackRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : PlaybackRepository {
        override suspend fun setDeviceProfile(profile: DeviceProfile) {
            client.setDeviceProfile(profile)
        }

        override suspend fun sources(mediaId: String): List<MediaSourceView> = client.mediaSources(mediaId)

        override suspend fun selectSource(
            mediaId: String,
            policy: QualityPolicy,
        ): SourceSelection = client.selectPlaybackSource(mediaId, policy)

        override suspend fun downgradeSource(
            titleId: String,
            currentFileId: String,
        ): SourceSelection? = client.selectDowngradeSource(titleId, currentFileId)

        override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig = client.playbackConfig(fileId)

        override suspend fun playbackPlan(
            titleId: String?,
            fileId: String,
        ): PlaybackPlan = client.playbackPlan(titleId, fileId)

        override suspend fun markWatched(id: String) = client.markWatched(id)

        override suspend fun markUnwatched(id: String) = client.markUnwatched(id)

        override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> =
            client.continueWatching(limit)

        override suspend fun history(
            first: UInt?,
            after: String?,
        ): HistoryPage = client.history(first, after)

        // A sample refused for credentials is queued rather than thrown; a
        // `401` behind it has already been announced by the core.
        override suspend fun reportProgress(
            fileId: String,
            positionSecs: Double,
            durationSecs: Double?,
            force: Boolean,
        ): ProgressOutcome = client.reportProgress(fileId, positionSecs, durationSecs, force)

        // A refused session is named, not thrown; the core announced the `401`.
        override suspend fun drainProgress(): ProgressDrain = client.drainProgress()

        override fun progressRetryDelaySecs(attempt: UInt): ULong =
            uniffi.beam_client_core.progressRetryDelaySecs(attempt)
    }

@Singleton
internal class BeamTelemetryRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : TelemetryRepository {
        override fun setClientKind(kind: TelemetryClientKind) = client.setPlaybackClientKind(kind)

        override fun recordStart(
            fileId: String,
            audioTrackIndex: UInt?,
        ): UInt = client.recordPlaybackStart(fileId, audioTrackIndex)

        override fun recordStartFailure(
            fileId: String,
            cause: StartFailureCause,
            audioTrackIndex: UInt?,
        ): UInt = client.recordPlaybackStartFailure(fileId, cause, audioTrackIndex)

        override fun recordRebuffer(
            fileId: String,
            durationMs: ULong,
        ): UInt = client.recordPlaybackRebuffer(fileId, durationMs)

        override fun recordSourceSwitch(
            fromFileId: String,
            toFileId: String,
            trigger: SwitchTrigger,
        ): UInt = client.recordPlaybackSourceSwitch(fromFileId, toFileId, trigger)

        override suspend fun flush(): TelemetryFlush = client.flushPlaybackTelemetry()
    }

@Singleton
internal class BeamSessionRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : SessionRepository {
        override suspend fun sessions(): List<DeviceSession> = client.sessions()

        override suspend fun revoke(sessionId: String) = client.revokeSession(sessionId)

        override suspend fun logoutEverywhere() = client.logoutEverywhere()
    }

@Singleton
internal class BeamAdminRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : AdminRepository {
        override suspend fun status(): AdminStatus = client.adminStatus()

        override suspend fun health(): ServerHealth = client.health()

        override suspend fun users(
            first: UInt?,
            after: String?,
        ): AdminUserPage = client.adminUsers(first, after)

        override suspend fun setUserDisabled(
            userId: String,
            disabled: Boolean,
        ) = client.setUserDisabled(userId, disabled)

        override suspend fun logs(
            first: UInt?,
            after: String?,
        ): AdminLogPage = client.adminLogs(first, after)

        override suspend fun events(limit: UInt?): List<AdminEvent> = client.adminEvents(limit)

        override suspend fun createLibrary(
            name: String,
            rootPath: String,
        ): LibrarySummary = client.createLibrary(name, rootPath)

        override suspend fun deleteLibrary(libraryId: String) = client.deleteLibrary(libraryId)

        override suspend fun scanLibrary(libraryId: String) = client.scanLibrary(libraryId)

        override suspend fun scanJob(libraryId: String): ScanJob? = client.libraryScan(libraryId)

        override suspend fun refreshMetadata(mediaId: String) = client.refreshMediaMetadata(mediaId)

        override suspend fun playbackTelemetry(
            from: String?,
            to: String?,
        ): PlaybackTelemetrySummary = client.adminPlaybackTelemetry(from, to)
    }
