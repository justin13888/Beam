package dev.beam.android.core.ffi.repository

import kotlinx.coroutines.flow.Flow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogEntry
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
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

// Every screen depends on one of these interfaces rather than on `BeamClient`
// directly. That is what lets a feature's tests run on the JVM with a fake,
// with no `.so` loaded and no JNA on the classpath -- which in turn is what
// makes the Android test suite runnable in CI without an emulator.

/** Which servers are known, and who we are on them. */
public interface ServerRepository {
    /** The known servers, re-emitted whenever one is added, chosen or removed. */
    public val servers: Flow<List<ServerSummary>>

    /** Load the registry and any stored sessions. Called once at app start. */
    public suspend fun restore(): List<ServerSummary>

    /** Add a server by address, and make it the active one. */
    public suspend fun addServer(baseUrl: String, displayName: String?): ServerSummary

    /** Make an already-known server the active one. */
    public suspend fun selectServer(serverId: String)

    /** Forget a server, its session, and its queued progress. */
    public suspend fun removeServer(serverId: String)

    /** The URL to open in the in-app browser to sign in. */
    public suspend fun loginUrl(serverId: String): String

    /** Hand over a cookie lifted from the browser, and verify it. */
    public suspend fun completeLogin(serverId: String, sessionCookie: String): UserSummary

    /** End the session on this device. */
    public suspend fun logout(serverId: String)

    /** The current authentication state of a server. */
    public suspend fun sessionState(serverId: String): SessionState

    /** The active server, or null when none is chosen. */
    public suspend fun activeServer(): ServerSummary?
}

/** Browsing the catalog. */
public interface CatalogRepository {
    /** One page of the catalog, filtered and sorted. */
    public suspend fun browse(query: BrowseQuery): MediaPage

    /** Everything a detail screen shows for one title. */
    public suspend fun detail(mediaId: String): MediaDetail

    /** Every genre in the catalog, for the filter chips. */
    public suspend fun genres(): List<String>

    /** Every library on the server. */
    public suspend fun libraries(): List<LibrarySummary>

    /** One library. */
    public suspend fun library(libraryId: String): LibrarySummary

    /** The files indexed into one library. */
    public suspend fun libraryFiles(libraryId: String): List<LibraryFileSummary>

    /** The next playable episode after this one, across season boundaries. */
    public suspend fun upNext(showId: String, currentEpisodeId: String): EpisodeSummary?
}

/** Choosing what to play, playing it, and recording where the viewer got to. */
public interface PlaybackRepository {
    /** Tell the core what this device can decode. */
    public suspend fun setDeviceProfile(profile: DeviceProfile)

    /** Every file behind a title, playable or not. */
    public suspend fun sources(mediaId: String): List<MediaSourceView>

    /** The file to play, plus why, plus why each other file was rejected. */
    public suspend fun selectSource(mediaId: String, policy: QualityPolicy): SourceSelection

    /** URL, headers and pins for the platform player to fetch bytes itself. */
    public suspend fun playbackConfig(fileId: String): PlaybackHttpConfig

    /** Partially-watched titles, ready to resume. */
    public suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry>

    /** One page of watch history. */
    public suspend fun history(limit: UInt?, offset: UInt?): HistoryPage

    /** Report where the viewer is, subject to the shared throttle. */
    public suspend fun reportProgress(
        fileId: String,
        positionSecs: Double,
        durationSecs: Double?,
        force: Boolean,
    ): ProgressOutcome

    /** Send every queued position that is due. Returns how many landed. */
    public suspend fun flushProgress(): UInt

    /** How many positions are waiting to be sent. */
    public suspend fun pendingProgressCount(): UInt
}

/** The signed-in devices on the profile screen. */
public interface SessionRepository {
    /** Every device signed in as this user. */
    public suspend fun sessions(): List<DeviceSession>

    /** Revoke one device. */
    public suspend fun revoke(sessionId: String)

    /** End every session everywhere, including this one. */
    public suspend fun logoutEverywhere()
}

/** The administrative surface, available only to administrators. */
public interface AdminRepository {
    /** The dashboard snapshot. */
    public suspend fun status(): AdminStatus

    /** The server's own health report. */
    public suspend fun health(): ServerHealth

    /** One page of user accounts. */
    public suspend fun users(limit: UInt?, offset: UInt?): AdminUserPage

    /** Block or unblock an account. */
    public suspend fun setUserDisabled(userId: String, disabled: Boolean)

    /** One page of the operational log. */
    public suspend fun logs(limit: UInt?, offset: UInt?): List<AdminLogEntry>

    /** How many log lines the server holds. */
    public suspend fun logCount(): ULong

    /** Recent server events, newest first. */
    public suspend fun events(limit: UInt?): List<AdminEvent>

    /** Create a library from a path on the server. */
    public suspend fun createLibrary(name: String, rootPath: String): LibrarySummary

    /** Delete a library and everything indexed into it. */
    public suspend fun deleteLibrary(libraryId: String)

    /** Rescan a library. Returns how many files were added. */
    public suspend fun scanLibrary(libraryId: String): UInt

    /** Re-fetch metadata for one title. */
    public suspend fun refreshMetadata(mediaId: String)
}
