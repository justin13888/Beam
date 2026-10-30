package dev.beam.android.core.ffi.repository

import kotlinx.coroutines.flow.Flow
import uniffi.beam_client_core.AdminEvent
import uniffi.beam_client_core.AdminLogPage
import uniffi.beam_client_core.AdminStatus
import uniffi.beam_client_core.AdminUserPage
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
    public suspend fun addServer(
        baseUrl: String,
        displayName: String?,
    ): ServerSummary

    /** Make an already-known server the active one. */
    public suspend fun selectServer(serverId: String)

    /** Forget a server, its session, and its queued progress. */
    public suspend fun removeServer(serverId: String)

    /** The URL to open in the in-app browser to sign in. */
    public suspend fun loginUrl(serverId: String): String

    /**
     * Begin signing in without a browser, by the device authorization grant.
     *
     * Throws a non-retryable 501 when the server's identity provider does not
     * offer the grant, which is the cue to fall back to [loginUrl].
     */
    public suspend fun startDeviceLogin(serverId: String): DeviceLoginPrompt

    /** Poll a device login once; signs in on approval. */
    public suspend fun pollDeviceLogin(
        serverId: String,
        deviceHandle: String,
    ): DeviceLoginStep

    /** Hand over a cookie lifted from the browser, and verify it. */
    public suspend fun completeLogin(
        serverId: String,
        sessionCookie: String,
    ): UserSummary

    /** End the session on this device. */
    public suspend fun logout(serverId: String)

    /**
     * Accept a certificate the verifier turned away.
     *
     * Only ever called after the viewer has been shown the certificate and has
     * agreed to it.
     */
    public suspend fun trustCertificate(
        serverId: String,
        fingerprint: String,
    )

    /** Withdraw trust from every certificate accepted for a server. */
    public suspend fun forgetCertificates(serverId: String)

    /** The certificates the viewer has accepted for a server. */
    public suspend fun trustedCertificates(serverId: String): List<String>

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
    public suspend fun upNext(
        showId: String,
        currentEpisodeId: String,
    ): EpisodeSummary?
}

/** Choosing what to play, playing it, and recording where the viewer got to. */
public interface PlaybackRepository {
    /** Tell the core what this device can decode. */
    public suspend fun setDeviceProfile(profile: DeviceProfile)

    /** Every file behind a title, playable or not. */
    public suspend fun sources(mediaId: String): List<MediaSourceView>

    /** The file to play, plus why, plus why each other file was rejected. */
    public suspend fun selectSource(
        mediaId: String,
        policy: QualityPolicy,
    ): SourceSelection

    /**
     * A smaller file of [titleId] (a movie or an episode) to fall back to after
     * sustained rebuffering on [currentFileId], or null when nothing smaller
     * plays on this device.
     */
    public suspend fun downgradeSource(
        titleId: String,
        currentFileId: String,
    ): SourceSelection?

    /** URL, headers and pins for the platform player to fetch bytes itself. */
    public suspend fun playbackConfig(fileId: String): PlaybackHttpConfig

    /**
     * Every part of the source [fileId] belongs to, in order, with which to
     * start in, and the headers and pins to fetch them with.
     *
     * [titleId] is the movie or episode the file belongs to; null, or a
     * lookup that fails, plans [fileId] alone.
     */
    public suspend fun playbackPlan(
        titleId: String?,
        fileId: String,
    ): PlaybackPlan

    /** Mark a movie, an episode, a season or a whole show watched. */
    public suspend fun markWatched(id: String)

    /** Mark a movie, an episode, a season or a whole show unwatched. */
    public suspend fun markUnwatched(id: String)

    /**
     * Partially-watched titles to resume, and the next episode to start of
     * each show the viewer finished an episode of
     * ([ContinueWatchingEntry.isNextUp]), newest first.
     */
    public suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry>

    /** One page of watch history: [first] rows after the page whose end cursor is [after]. */
    public suspend fun history(
        first: UInt?,
        after: String?,
    ): HistoryPage

    /** Report where the viewer is, subject to the shared throttle. */
    public suspend fun reportProgress(
        fileId: String,
        positionSecs: Double,
        durationSecs: Double?,
        force: Boolean,
    ): ProgressOutcome

    /**
     * Send every queued position that is due, to the active server. Returns
     * how many landed.
     */
    public suspend fun flushProgress(): UInt

    /**
     * How many positions are still queued for the active server. A drain
     * stops at the first failed send, so this -- not [flushProgress]'s count
     * -- is what says whether anything is left to retry.
     */
    public suspend fun pendingProgressCount(): UInt
}

/**
 * Playback telemetry: what started, what failed to start, what stalled, and
 * what switched source.
 *
 * The vocabulary, the batching and what each server answer means are the
 * core's; this side reports what the player saw. Every `record` call is local
 * and cheap -- nothing leaves the device until [flush] -- and answers how many
 * events are waiting, so the player can flush once a request's worth has built
 * up. Off by default on the server: once it answers that telemetry is off,
 * every call here is a no-op for the rest of the session.
 */
public interface TelemetryRepository {
    /** Which kind of client this is, for every report. Set once. */
    public fun setClientKind(kind: TelemetryClientKind)

    /** A playback's first frame is on screen. */
    public fun recordStart(
        fileId: String,
        audioTrackIndex: UInt?,
    ): UInt

    /** A playback failed before its first frame. */
    public fun recordStartFailure(
        fileId: String,
        cause: StartFailureCause,
        audioTrackIndex: UInt?,
    ): UInt

    /** Playback stalled mid-stream, after its first frame, for [durationMs]. */
    public fun recordRebuffer(
        fileId: String,
        durationMs: ULong,
    ): UInt

    /** Playback moved from one file of a title to another. */
    public fun recordSourceSwitch(
        fromFileId: String,
        toFileId: String,
        trigger: SwitchTrigger,
    ): UInt

    /** Report everything waiting. */
    public suspend fun flush(): TelemetryFlush
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

    /** One page of user accounts: [first] of them, after the page whose `endCursor` is [after]. */
    public suspend fun users(
        first: UInt?,
        after: String?,
    ): AdminUserPage

    /** Block or unblock an account. */
    public suspend fun setUserDisabled(
        userId: String,
        disabled: Boolean,
    )

    /**
     * One page of the operational log, newest first: [first] lines, after the page whose
     * `endCursor` is [after]. Its `total` is how many lines the server holds.
     */
    public suspend fun logs(
        first: UInt?,
        after: String?,
    ): AdminLogPage

    /** Recent server events, newest first. */
    public suspend fun events(limit: UInt?): List<AdminEvent>

    /** Create a library from a path on the server. */
    public suspend fun createLibrary(
        name: String,
        rootPath: String,
    ): LibrarySummary

    /** Delete a library and everything indexed into it. */
    public suspend fun deleteLibrary(libraryId: String)

    /**
     * Start a rescan of a library. Returns once the server has accepted it; the
     * scan itself runs on the server afterwards.
     */
    public suspend fun scanLibrary(libraryId: String)

    /**
     * The latest scan of a library since the server started, running or
     * finished; null when it has not been scanned since.
     */
    public suspend fun scanJob(libraryId: String): ScanJob?

    /** Re-fetch metadata for one title. */
    public suspend fun refreshMetadata(mediaId: String)

    /**
     * Playback telemetry from [from] to [to], inclusive UTC days as
     * `YYYY-MM-DD`; null for the server's default of the last thirty days.
     */
    public suspend fun playbackTelemetry(
        from: String?,
        to: String?,
    ): PlaybackTelemetrySummary
}
