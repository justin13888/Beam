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
    private var metadata: [String: DownloadMetadata] = [:]
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
        loadMetadata()
        rebuildRecords()
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
    public func enqueue(fileId: String, title: String, subtitle: String?, sizeBytes: UInt64?) {
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

        let task = backgroundSession(for: config).downloadTask(with: request)
        // `taskDescription` is the only field that survives the app being
        // relaunched to finish a background transfer, so the file id rides
        // there rather than in a dictionary that would be empty on relaunch.
        task.taskDescription = fileId
        tasksByFile[fileId] = task

        metadata[fileId] = DownloadMetadata(
            fileId: fileId,
            title: title,
            subtitle: subtitle,
            totalBytes: sizeBytes,
            fileName: url.lastPathComponent
        )
        saveMetadata()
        rebuildRecords(overriding: [fileId: .queued])
        task.resume()
    }

    /// Stop and forget a download, deleting any bytes already written.
    public func remove(fileId: String) {
        tasksByFile.removeValue(forKey: fileId)?.cancel()
        if let url = localURL(for: fileId) {
            try? FileManager.default.removeItem(at: url)
        }
        metadata.removeValue(forKey: fileId)
        saveMetadata()
        rebuildRecords()
    }

    /// Pause a transfer, keeping what has been written.
    public func pause(fileId: String) {
        tasksByFile[fileId]?.suspend()
        rebuildRecords(overriding: [fileId: .paused(fraction: nil)])
    }

    /// Resume a paused transfer.
    public func resume(fileId: String) {
        tasksByFile[fileId]?.resume()
        rebuildRecords(overriding: [fileId: .downloading(fraction: nil)])
    }

    /// Where a completed download lives, or `nil` when it is not on disk.
    public func localURL(for fileId: String) -> URL? {
        guard let entry = metadata[fileId] else { return nil }
        let url = directory.appendingPathComponent(entry.storedName)
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

    private func backgroundSession(for config: PlaybackHttpConfig) -> URLSession {
        if let session { return session }
        let configuration = URLSessionConfiguration.background(
            withIdentifier: "net.justinchung.beam.downloads"
        )
        configuration.allowsCellularAccess = allowsCellular
        configuration.isDiscretionary = false
        configuration.sessionSendsLaunchEvents = true
        let created = URLSession(
            configuration: configuration,
            delegate: DownloadDelegate(coordinator: self, config: config),
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
        guard let entry = metadata[fileId] else { return }
        let destination = directory.appendingPathComponent(entry.storedName)
        try? FileManager.default.removeItem(at: destination)
        do {
            try FileManager.default.moveItem(at: location, to: destination)
        } catch {
            tasksByFile.removeValue(forKey: fileId)
            rebuildRecords(overriding: [fileId: .failed(error.localizedDescription)])
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
        rebuildRecords()
    }

    fileprivate func progressed(fileId: String, received: Int64, expected: Int64) {
        let fraction = expected > 0 ? Double(received) / Double(expected) : nil
        rebuildRecords(overriding: [fileId: .downloading(fraction: fraction)], received: received)
    }

    fileprivate func failed(fileId: String, message: String) {
        tasksByFile.removeValue(forKey: fileId)
        rebuildRecords(overriding: [fileId: .failed(message)])
    }

    private func rebuildRecords(
        overriding overrides: [String: DownloadState] = [:],
        received: Int64 = 0
    ) {
        records = metadata.values
            .sorted { $0.title < $1.title }
            .map { entry in
                let url = localURL(for: entry.fileId)
                let state =
                    overrides[entry.fileId] ?? (url != nil ? .completed : .queued)
                return DownloadRecord(
                    fileId: entry.fileId,
                    title: entry.title,
                    subtitle: entry.subtitle,
                    totalBytes: entry.totalBytes,
                    receivedBytes: UInt64(max(0, received)),
                    state: state,
                    localURL: url
                )
            }
    }

    private var metadataURL: URL {
        directory.appendingPathComponent("downloads.json")
    }

    private func loadMetadata() {
        guard let data = try? Data(contentsOf: metadataURL),
            let entries = try? JSONDecoder().decode([String: DownloadMetadata].self, from: data)
        else {
            return
        }
        metadata = entries
    }

    private func saveMetadata() {
        guard let data = try? JSONEncoder().encode(metadata) else { return }
        try? data.write(to: metadataURL, options: .atomic)
    }
}

/// What the downloads screen needs that the file itself does not carry.
private struct DownloadMetadata: Codable {
    let fileId: String
    let title: String
    let subtitle: String?
    let totalBytes: UInt64?
    let fileName: String

    /// The name on disk.
    ///
    /// Keyed by file id rather than by the server's filename: two titles can
    /// share a filename, and the extension is kept because both engines and
    /// `EngineSelector` read it when the catalogue did not record a container.
    var storedName: String {
        let ext = (fileName as NSString).pathExtension
        return ext.isEmpty ? fileId : "\(fileId).\(ext)"
    }
}

/// Bridges `URLSession`'s callbacks back to the coordinator.
private final class DownloadDelegate: NSObject, URLSessionDownloadDelegate, @unchecked Sendable {
    private weak var coordinator: DownloadCoordinator?
    private let trust: TrustingSessionDelegate

    init(coordinator: DownloadCoordinator, config: PlaybackHttpConfig) {
        self.coordinator = coordinator
        self.trust = TrustingSessionDelegate(
            evaluator: CertificateTrustEvaluator(
                trustedFingerprints: config.trustedFingerprints,
                pinnedHost: config.pinnedHost
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
