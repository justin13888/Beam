package dev.beam.android.core.ffi.repository

import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
import uniffi.beam_client_core.BeamClient
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
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The server registry, over the core.
 *
 * The core holds the registry as plain state rather than as a stream, so this
 * re-emits the whole list after any mutation. A media app has a handful of
 * servers at most, which makes a diffing protocol across the FFI boundary
 * more machinery than the problem deserves.
 */
@Singleton
internal class BeamServerRepository @Inject constructor(
    private val client: BeamClient,
) : ServerRepository {

    private val changes = MutableSharedFlow<List<ServerSummary>>(replay = 1)

    override val servers: Flow<List<ServerSummary>> = changes.asSharedFlow()

    override suspend fun restore(): List<ServerSummary> =
        client.restore().also { changes.emit(it) }

    override suspend fun addServer(baseUrl: String, displayName: String?): ServerSummary =
        client.addServer(baseUrl, displayName).also { republish() }

    override suspend fun selectServer(serverId: String) {
        client.selectServer(serverId)
        republish()
    }

    override suspend fun removeServer(serverId: String) {
        client.removeServer(serverId)
        republish()
    }

    override suspend fun loginUrl(serverId: String): String = client.loginUrl(serverId)

    override suspend fun completeLogin(serverId: String, sessionCookie: String): UserSummary =
        client.completeLogin(serverId, sessionCookie).also { republish() }

    override suspend fun logout(serverId: String) {
        client.logout(serverId)
        republish()
    }

    override suspend fun sessionState(serverId: String): SessionState =
        client.sessionState(serverId)

    override suspend fun activeServer(): ServerSummary? =
        client.listServers().firstOrNull { it.isActive }

    private suspend fun republish() {
        changes.emit(client.listServers())
    }
}

@Singleton
internal class BeamCatalogRepository @Inject constructor(
    private val client: BeamClient,
) : CatalogRepository {
    override suspend fun browse(query: BrowseQuery): MediaPage = client.browseMedia(query)
    override suspend fun detail(mediaId: String): MediaDetail = client.mediaDetail(mediaId)
    override suspend fun genres(): List<String> = client.genres()
    override suspend fun libraries(): List<LibrarySummary> = client.libraries()
    override suspend fun library(libraryId: String): LibrarySummary = client.library(libraryId)
    override suspend fun libraryFiles(libraryId: String): List<LibraryFileSummary> =
        client.libraryFiles(libraryId)

    override suspend fun upNext(showId: String, currentEpisodeId: String): EpisodeSummary? =
        client.upNextInShow(showId, currentEpisodeId)
}

@Singleton
internal class BeamPlaybackRepository @Inject constructor(
    private val client: BeamClient,
) : PlaybackRepository {
    override suspend fun setDeviceProfile(profile: DeviceProfile) {
        client.setDeviceProfile(profile)
    }

    override suspend fun sources(mediaId: String): List<MediaSourceView> =
        client.mediaSources(mediaId)

    override suspend fun selectSource(mediaId: String, policy: QualityPolicy): SourceSelection =
        client.selectPlaybackSource(mediaId, policy)

    override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig =
        client.playbackConfig(fileId)

    override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> =
        client.continueWatching(limit)

    override suspend fun history(limit: UInt?, offset: UInt?): HistoryPage =
        client.history(limit, offset)

    override suspend fun reportProgress(
        fileId: String,
        positionSecs: Double,
        durationSecs: Double?,
        force: Boolean,
    ): ProgressOutcome = client.reportProgress(fileId, positionSecs, durationSecs, force)

    override suspend fun flushProgress(): UInt = client.flushProgress()

    override suspend fun pendingProgressCount(): UInt = client.pendingProgressCount()
}

@Singleton
internal class BeamSessionRepository @Inject constructor(
    private val client: BeamClient,
) : SessionRepository {
    override suspend fun sessions(): List<DeviceSession> = client.sessions()
    override suspend fun revoke(sessionId: String) = client.revokeSession(sessionId)
    override suspend fun logoutEverywhere() = client.logoutEverywhere()
}

@Singleton
internal class BeamAdminRepository @Inject constructor(
    private val client: BeamClient,
) : AdminRepository {
    override suspend fun status(): AdminStatus = client.adminStatus()
    override suspend fun health(): ServerHealth = client.health()
    override suspend fun users(limit: UInt?, offset: UInt?): AdminUserPage =
        client.adminUsers(limit, offset)

    override suspend fun setUserDisabled(userId: String, disabled: Boolean) =
        client.setUserDisabled(userId, disabled)

    override suspend fun logs(limit: UInt?, offset: UInt?): List<AdminLogEntry> =
        client.adminLogs(limit, offset)

    override suspend fun logCount(): ULong = client.adminLogCount()
    override suspend fun events(limit: UInt?): List<AdminEvent> = client.adminEvents(limit)

    override suspend fun createLibrary(name: String, rootPath: String): LibrarySummary =
        client.createLibrary(name, rootPath)

    override suspend fun deleteLibrary(libraryId: String) = client.deleteLibrary(libraryId)
    override suspend fun scanLibrary(libraryId: String): UInt = client.scanLibrary(libraryId)
    override suspend fun refreshMetadata(mediaId: String) = client.refreshMediaMetadata(mediaId)
}
