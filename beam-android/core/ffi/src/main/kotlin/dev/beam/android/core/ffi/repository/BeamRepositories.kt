package dev.beam.android.core.ffi.repository

import dev.beam.android.core.ffi.toFailure
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogPage
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.BeamException
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
import uniffi.beam_client_core.ProgressOutcome
import uniffi.beam_client_core.QualityPolicy
import uniffi.beam_client_core.ScanJob
import uniffi.beam_client_core.ServerHealth
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SourceSelection
import uniffi.beam_client_core.StartFailureCause
import uniffi.beam_client_core.SwitchTrigger
import uniffi.beam_client_core.TelemetryClientKind
import uniffi.beam_client_core.TelemetryFlush
import uniffi.beam_client_core.UserSummary
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The server list as the shell observes it, and the one place it is
 * re-published from.
 *
 * Shared by every repository rather than owned by [BeamServerRepository],
 * because a session does not only change through the registry: any secured
 * call the server answers with a `401` expires the session inside the core,
 * and the shell can only route the viewer back to sign-in if that reaches
 * the list it watches.
 */
@Singleton
internal class ServerChanges
    @Inject
    constructor(
        private val client: BeamClient,
    ) {
        private val changes = MutableSharedFlow<List<ServerSummary>>(replay = 1)

        val servers: Flow<List<ServerSummary>> = changes.asSharedFlow()

        suspend fun emit(servers: List<ServerSummary>) = changes.emit(servers)

        suspend fun republish() = changes.emit(client.listServers())

        /**
         * Re-publish on the side of another call, whose own answer is what
         * its caller is waiting for: a registry that cannot be read now is
         * read again by the next change, and must not replace that answer.
         */
        suspend fun republishQuietly() {
            try {
                republish()
            } catch (_: BeamException) {
            }
        }

        /**
         * Run one core call, re-publishing the list when it failed for want
         * of a session.
         *
         * The core has no observer channel -- the foreign side reads the state
         * -- so a failure whose answer is to sign in is the signal that the
         * session may have changed under the shell.
         */
        suspend fun <T> watching(call: suspend () -> T): T =
            try {
                call()
            } catch (failure: BeamException) {
                if (failure.toFailure().requiresSignIn) republishQuietly()
                throw failure
            }
    }

/**
 * The server registry, over the core.
 *
 * The core holds the registry as plain state rather than as a stream, so this
 * re-emits the whole list after any mutation. A media app has a handful of
 * servers at most, which makes a diffing protocol across the FFI boundary
 * more machinery than the problem deserves.
 */
@Singleton
internal class BeamServerRepository
    @Inject
    constructor(
        private val client: BeamClient,
        private val changes: ServerChanges,
    ) : ServerRepository {
        override val servers: Flow<List<ServerSummary>> = changes.servers

        override suspend fun restore(): List<ServerSummary> = client.restore().also { changes.emit(it) }

        override suspend fun addServer(
            baseUrl: String,
            displayName: String?,
        ): ServerSummary = client.addServer(baseUrl, displayName).also { republish() }

        override suspend fun selectServer(serverId: String) {
            client.selectServer(serverId)
            republish()
        }

        override suspend fun removeServer(serverId: String) {
            client.removeServer(serverId)
            republish()
        }

        override suspend fun loginUrl(serverId: String): String = client.loginUrl(serverId)

        override suspend fun startDeviceLogin(serverId: String): DeviceLoginPrompt = client.startDeviceLogin(serverId)

        override suspend fun pollDeviceLogin(
            serverId: String,
            deviceHandle: String,
        ): DeviceLoginStep =
            client.pollDeviceLogin(serverId, deviceHandle).also {
                if (it is DeviceLoginStep.SignedIn) republish()
            }

        override suspend fun completeLogin(
            serverId: String,
            sessionCookie: String,
        ): UserSummary = client.completeLogin(serverId, sessionCookie).also { republish() }

        override suspend fun logout(serverId: String) {
            client.logout(serverId)
            republish()
        }

        override suspend fun trustCertificate(
            serverId: String,
            fingerprint: String,
        ) {
            client.trustCertificate(serverId, fingerprint)
        }

        override suspend fun forgetCertificates(serverId: String) {
            client.forgetCertificates(serverId)
            republish()
        }

        override suspend fun trustedCertificates(serverId: String): List<String> = client.trustedCertificates(serverId)

        override suspend fun activeServer(): ServerSummary? = client.listServers().firstOrNull { it.isActive }

        private suspend fun republish() = changes.republish()
    }

@Singleton
internal class BeamCatalogRepository
    @Inject
    constructor(
        private val client: BeamClient,
        private val changes: ServerChanges,
    ) : CatalogRepository {
        override suspend fun browse(query: BrowseQuery): MediaPage = changes.watching { client.browseMedia(query) }

        override suspend fun detail(mediaId: String): MediaDetail = changes.watching { client.mediaDetail(mediaId) }

        override suspend fun genres(): List<String> = changes.watching { client.genres() }

        override suspend fun libraries(): List<LibrarySummary> = changes.watching { client.libraries() }

        override suspend fun library(libraryId: String): LibrarySummary = changes.watching { client.library(libraryId) }

        override suspend fun libraryFiles(libraryId: String): List<LibraryFileSummary> =
            changes.watching { client.libraryFiles(libraryId) }

        override suspend fun upNext(
            showId: String,
            currentEpisodeId: String,
        ): EpisodeSummary? = changes.watching { client.upNextInShow(showId, currentEpisodeId) }
    }

@Singleton
internal class BeamPlaybackRepository
    @Inject
    constructor(
        private val client: BeamClient,
        private val changes: ServerChanges,
    ) : PlaybackRepository {
        override suspend fun setDeviceProfile(profile: DeviceProfile) {
            client.setDeviceProfile(profile)
        }

        override suspend fun sources(mediaId: String): List<MediaSourceView> =
            changes.watching { client.mediaSources(mediaId) }

        override suspend fun selectSource(
            mediaId: String,
            policy: QualityPolicy,
        ): SourceSelection = changes.watching { client.selectPlaybackSource(mediaId, policy) }

        override suspend fun downgradeSource(
            titleId: String,
            currentFileId: String,
        ): SourceSelection? = changes.watching { client.selectDowngradeSource(titleId, currentFileId) }

        override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig =
            changes.watching { client.playbackConfig(fileId) }

        override suspend fun playbackPlan(
            titleId: String?,
            fileId: String,
        ): PlaybackPlan = changes.watching { client.playbackPlan(titleId, fileId) }

        override suspend fun markWatched(id: String) = changes.watching { client.markWatched(id) }

        override suspend fun markUnwatched(id: String) = changes.watching { client.markUnwatched(id) }

        override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> =
            changes.watching { client.continueWatching(limit) }

        override suspend fun history(
            first: UInt?,
            after: String?,
        ): HistoryPage = changes.watching { client.history(first, after) }

        // A sample refused for credentials is queued rather than thrown, so a
        // `Queued` answer is also when the session may have just expired.
        override suspend fun reportProgress(
            fileId: String,
            positionSecs: Double,
            durationSecs: Double?,
            force: Boolean,
        ): ProgressOutcome =
            changes.watching { client.reportProgress(fileId, positionSecs, durationSecs, force) }.also {
                if (it is ProgressOutcome.Queued) changes.republishQuietly()
            }

        override suspend fun flushProgress(): UInt = changes.watching { client.flushProgress() }
    }

@Singleton
internal class BeamTelemetryRepository
    @Inject
    constructor(
        private val client: BeamClient,
        private val changes: ServerChanges,
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

        override suspend fun flush(): TelemetryFlush = changes.watching { client.flushPlaybackTelemetry() }
    }

@Singleton
internal class BeamSessionRepository
    @Inject
    constructor(
        private val client: BeamClient,
        private val changes: ServerChanges,
    ) : SessionRepository {
        override suspend fun sessions(): List<DeviceSession> = changes.watching { client.sessions() }

        override suspend fun revoke(sessionId: String) = changes.watching { client.revokeSession(sessionId) }

        override suspend fun logoutEverywhere() = changes.watching { client.logoutEverywhere() }
    }

@Singleton
internal class BeamAdminRepository
    @Inject
    constructor(
        private val client: BeamClient,
        private val changes: ServerChanges,
    ) : AdminRepository {
        override suspend fun status(): AdminStatus = changes.watching { client.adminStatus() }

        override suspend fun health(): ServerHealth = changes.watching { client.health() }

        override suspend fun users(
            first: UInt?,
            after: String?,
        ): AdminUserPage = changes.watching { client.adminUsers(first, after) }

        override suspend fun setUserDisabled(
            userId: String,
            disabled: Boolean,
        ) = changes.watching { client.setUserDisabled(userId, disabled) }

        override suspend fun logs(
            first: UInt?,
            after: String?,
        ): AdminLogPage = changes.watching { client.adminLogs(first, after) }

        override suspend fun events(limit: UInt?): List<AdminEvent> = changes.watching { client.adminEvents(limit) }

        override suspend fun createLibrary(
            name: String,
            rootPath: String,
        ): LibrarySummary = changes.watching { client.createLibrary(name, rootPath) }

        override suspend fun deleteLibrary(libraryId: String) = changes.watching { client.deleteLibrary(libraryId) }

        override suspend fun scanLibrary(libraryId: String) = changes.watching { client.scanLibrary(libraryId) }

        override suspend fun scanJob(libraryId: String): ScanJob? = changes.watching { client.libraryScan(libraryId) }

        override suspend fun refreshMetadata(mediaId: String) =
            changes.watching { client.refreshMediaMetadata(mediaId) }

        override suspend fun playbackTelemetry(
            from: String?,
            to: String?,
        ): PlaybackTelemetrySummary = changes.watching { client.adminPlaybackTelemetry(from, to) }
    }
