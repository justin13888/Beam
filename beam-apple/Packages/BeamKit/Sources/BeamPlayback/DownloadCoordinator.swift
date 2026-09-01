import BeamCore
import BeamFFI
import BeamModel
import Foundation
import os

/// Offline downloads, over `URLSession`'s background configuration.
///
/// A background session rather than an in-process transfer, because the whole
/// point of a download is that it survives the app being suspended -- iOS will
/// suspend a foreground transfer within seconds of the app leaving the screen,
/// and a film is not a few seconds of transfer.
///
/// Both playback engines accept a `file://` URL, so a completed download plays
/// through exactly the code an online stream does -- an MKV through the
/// sample-buffer engine, an MP4 through `AVPlayer`. That is deliberate:
/// offline playback with its own code path is offline playback that breaks
/// separately.
@MainActor
@Observable
public final class DownloadCoordinator: NSObject {
    /// Everything currently downloaded or downloading, newest first.
    public private(set) var records: [DownloadRecord] = []

    private let playback: any PlaybackRepository
    private let directory: URL
    private var session: URLSession?
    private var tasksByFile: [String: URLSessionDownloadTask] = [:]
    private var allowsCellular: Bool

    /// A coordinator writing under Application Support.
    ///
    /// - Parameter directory: injected so a test writes into a temporary
    ///   directory rather than into whatever the simulator is holding.
    public init(
        playback: any PlaybackRepository,
        allowsCellular: Bool = false,
        directory: URL? = nil
    ) {
        self.playback = playback
        self.allowsCellular = allowsCellular
        let base =
            directory
            ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first?
            .appendingPathComponent("Downloads", isDirectory: true)
            ?? FileManager.default.temporaryDirectory
        self.directory = base
        super.init()

        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        Task { await refresh() }
    }

    /// Re-read the catalogue the core owns.
    ///
    /// The core is the only place a download's display fields and state live,
    /// so this is the only thing that populates `records`. A catalogue that
    /// cannot be read leaves the list as it was rather than blanking a screen
    /// the viewer is looking at.
    public func refresh() async {
        guard let latest = try? await playback.downloads() else { return }
        records = latest.sorted { $0.title < $1.title }
    }

    /// Whether transfers may run without Wi-Fi.
    ///
    /// Applied to new transfers only. `URLSessionConfiguration` is read when
    /// the session is created, so changing this cannot reach a transfer already
    /// in flight -- which is the honest behaviour: cancelling someone's
    /// half-finished download because they toggled a switch would lose the
    /// bytes.
    public func setAllowsCellular(_ allowed: Bool) {
        allowsCellular = allowed
    }

    /// Start downloading `fileId`.
    ///
    /// The identifiers travel with the record because the core catalogue is
    /// what a removed server is reconciled against, and because the downloads
    /// screen offers "play" straight into the title it belongs to.
    public func enqueue(
        fileId: String,
        serverId: String,
        mediaId: String,
        episodeId: String? = nil,
        title: String,
        subtitle: String?,
        posterUrl: String? = nil,
        sizeBytes: UInt64?
    ) {
        guard tasksByFile[fileId] == nil, !isDownloaded(fileId: fileId) else { return }
        guard let config = try? playback.playbackConfig(fileId: fileId),
            let url = downloadURL(from: config)
        else {
            return
        }

        var request = URLRequest(url: url)
        for (name, value) in config.headers {
            request.setValue(value, forHTTPHeaderField: name)
        }

        let task = backgroundSession().downloadTask(with: request)
        // `taskDescription` is the only field that survives the app being
        // relaunched to finish a background transfer, so the file id rides
        // there rather than in a dictionary that would be empty on relaunch.
        task.taskDescription = fileId
        tasksByFile[fileId] = task

        let record = DownloadRecord(
            fileId: fileId,
            serverId: serverId,
            mediaId: mediaId,
            episodeId: episodeId,
            title: title,
            subtitle: subtitle,
            posterUrl: posterUrl,
            fileName: url.lastPathComponent,
            state: .queued,
            downloadedBytes: 0,
            totalBytes: sizeBytes
        )
        Task {
            try? await playback.recordDownloadQueued(record)
            await refresh()
        }
        task.resume()
    }

    /// Stop and forget a download, deleting any bytes already written.
    public func remove(fileId: String) {
        tasksByFile.removeValue(forKey: fileId)?.cancel()
        if let url = localURL(for: fileId) {
            try? FileManager.default.removeItem(at: url)
        }
        Task {
            try? await playback.forgetDownload(fileId: fileId)
            await refresh()
        }
    }

    /// Pause a transfer, keeping what has been written.
    public func pause(fileId: String) {
        tasksByFile[fileId]?.suspend()
        Task {
            try? await playback.setDownloadState(fileId: fileId, state: .paused)
            await refresh()
        }
    }

    /// Resume a paused transfer.
    public func resume(fileId: String) {
        tasksByFile[fileId]?.resume()
        Task {
            try? await playback.setDownloadState(fileId: fileId, state: .downloading)
            await refresh()
        }
    }

    /// Where a completed download lives, or `nil` when it is not on disk.
    ///
    /// The name is composed by the core, so both clients store a file under the
    /// same name for the same download.
    public func localURL(for fileId: String) -> URL? {
        guard let record = records.first(where: { $0.fileId == fileId }) else { return nil }
        let url = directory.appendingPathComponent(downloadStoredName(record: record))
        return FileManager.default.fileExists(atPath: url.path) ? url : nil
    }

    /// Whether `fileId` is fully downloaded.
    public func isDownloaded(fileId: String) -> Bool {
        localURL(for: fileId) != nil
    }

    /// A playback item that reads from disk instead of the network.
    public func offlineItem(for request: PlaybackRequest, container: String?) -> PlaybackItem? {
        guard let url = localURL(for: request.fileId) else { return nil }
        return PlaybackItem(
            url: url,
            container: container,
            startPositionSeconds: request.startPositionSeconds,
            request: request
        )
    }

    // MARK: - Internals

    /// The one background session every download runs on.
    ///
    /// Built from `serverHTTPConfig()` rather than from any one file's
    /// `playbackConfig`. The credential and the trust decision are properties
    /// of the *server* -- the core says so, and only the URL is per-file -- and
    /// a session is created once and reused, so seeding its trust from
    /// whichever file happened to be downloaded first would apply that file's
    /// server's fingerprints to every later download. With one server that is
    /// invisible; with two it is a download that fails to validate a
    /// certificate the user has already accepted.
    private func backgroundSession() -> URLSession {
        if let session { return session }
        let configuration = URLSessionConfiguration.background(
            withIdentifier: "net.justinchung.beam.downloads"
        )
        configuration.allowsCellularAccess = allowsCellular
        configuration.isDiscretionary = false
        configuration.sessionSendsLaunchEvents = true

        let serverConfig = try? playback.serverHTTPConfig()
        let created = URLSession(
            configuration: configuration,
            delegate: DownloadDelegate(
                coordinator: self,
                trustedFingerprints: serverConfig?.trustedFingerprints ?? [],
                pinnedHost: serverConfig?.host ?? ""
            ),
            delegateQueue: nil
        )
        session = created
        return created
    }

    private func downloadURL(from config: PlaybackHttpConfig) -> URL? {
        // The core hands over the *stream* URL. Downloads want the download
        // route, which serves the same bytes with a filename attached, so the
        // suffix is swapped rather than the URL rebuilt from parts -- there is
        // no other place in the client that knows how to compose one.
        guard config.url.hasSuffix("/stream") else { return URL(string: config.url) }
        return URL(string: String(config.url.dropLast("stream".count)) + "download")
    }

    fileprivate func finished(fileId: String, movedFrom location: URL) {
        guard let record = records.first(where: { $0.fileId == fileId }) else { return }
        let destination = directory.appendingPathComponent(downloadStoredName(record: record))
        try? FileManager.default.removeItem(at: destination)
        do {
            try FileManager.default.moveItem(at: location, to: destination)
        } catch {
            tasksByFile.removeValue(forKey: fileId)
            let message = error.localizedDescription
            Task {
                try? await playback.setDownloadState(
                    fileId: fileId,
                    state: .failed(message: message)
                )
                await refresh()
            }
            return
        }
        // Downloads are explicit user choices, so they are excluded from
        // iCloud backup rather than from eviction: the system may not delete
        // them, and there is no reason to upload a film the server already has.
        var url = destination
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        try? url.setResourceValues(values)

        tasksByFile.removeValue(forKey: fileId)
        Task {
            try? await playback.setDownloadState(fileId: fileId, state: .completed)
            await refresh()
        }
    }

    fileprivate func progressed(fileId: String, received: Int64, expected: Int64) {
        // `expected` is -1 until the server has answered with a length. `nil`
        // means "not yet known", which the core will not unlearn once it is --
        // and the fraction itself is the core's to compute, so that this screen
        // and the Android one cannot disagree about it.
        let total = expected > 0 ? UInt64(expected) : nil
        Task {
            try? await playback.recordDownloadProgress(
                fileId: fileId,
                downloadedBytes: UInt64(max(0, received)),
                totalBytes: total
            )
            await refresh()
        }
    }

    fileprivate func failed(fileId: String, message: String) {
        tasksByFile.removeValue(forKey: fileId)
        Task {
            try? await playback.setDownloadState(
                fileId: fileId,
                state: .failed(message: message)
            )
            await refresh()
        }
    }

}

/// Bridges `URLSession`'s callbacks back to the coordinator.
private final class DownloadDelegate: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    private weak var coordinator: DownloadCoordinator?
    private let trust: TrustingSessionDelegate

    init(coordinator: DownloadCoordinator, trustedFingerprints: [String], pinnedHost: String) {
        self.coordinator = coordinator
        self.trust = TrustingSessionDelegate(
            evaluator: CertificateTrustEvaluator(
                trustedFingerprints: trustedFingerprints,
                pinnedHost: pinnedHost
            )
        )
    }

    func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didFinishDownloadingTo location: URL
    ) {
        guard let fileId = downloadTask.taskDescription else { return }
        // The file at `location` is deleted the moment this returns, so it is
        // moved synchronously here rather than hopped to the main actor first.
        let staged = location.deletingLastPathComponent()
            .appendingPathComponent("beam-\(fileId)")
        try? FileManager.default.removeItem(at: staged)
        try? FileManager.default.moveItem(at: location, to: staged)

        Task { @MainActor [weak coordinator] in
            coordinator?.finished(fileId: fileId, movedFrom: staged)
        }
    }

    func urlSession(
        _ session: URLSession,
        downloadTask: URLSessionDownloadTask,
        didWriteData bytesWritten: Int64,
        totalBytesWritten: Int64,
        totalBytesExpectedToWrite: Int64
    ) {
        guard let fileId = downloadTask.taskDescription else { return }
        Task { @MainActor [weak coordinator] in
            coordinator?.progressed(
                fileId: fileId,
                received: totalBytesWritten,
                expected: totalBytesExpectedToWrite
            )
        }
    }

    func urlSession(
        _ session: URLSession,
        task: URLSessionTask,
        didCompleteWithError error: Error?
    ) {
        guard let error, let fileId = task.taskDescription else { return }
        let message = error.localizedDescription
        Task { @MainActor [weak coordinator] in
            coordinator?.failed(fileId: fileId, message: message)
        }
    }

    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        trust.urlSession(session, didReceive: challenge, completionHandler: completionHandler)
    }
}
