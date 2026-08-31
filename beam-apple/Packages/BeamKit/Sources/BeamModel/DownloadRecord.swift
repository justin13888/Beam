import Foundation

/// Where one download has got to.
public enum DownloadState: Equatable, Sendable {
    /// Accepted, not yet started.
    case queued
    /// Transferring, with `fraction` in `0...1` where the total is known.
    case downloading(fraction: Double?)
    /// Paused by the user or by a policy such as "Wi-Fi only".
    case paused(fraction: Double?)
    /// On disk and playable offline.
    case completed
    /// Stopped for a reason worth showing.
    case failed(String)
}

/// One offline download, as the downloads screen renders it.
public struct DownloadRecord: Equatable, Identifiable, Sendable {
    /// The file being downloaded; also the identity of the download.
    public let fileId: String
    /// The title to show.
    public let title: String
    /// A second line.
    public let subtitle: String?
    /// Total size, where the server declared one.
    public let totalBytes: UInt64?
    /// Bytes on disk so far.
    public let receivedBytes: UInt64
    /// Where it has got to.
    public let state: DownloadState
    /// Where the bytes live, once there are any to play.
    public let localURL: URL?

    /// The file identifier doubles as the record's identity: a file is
    /// downloaded at most once.
    public var id: String { fileId }

    /// Progress in `0...1`, where the total size is known.
    public var fraction: Double? {
        guard let totalBytes, totalBytes > 0 else { return nil }
        return min(1, Double(receivedBytes) / Double(totalBytes))
    }

    /// Memberwise.
    public init(
        fileId: String,
        title: String,
        subtitle: String? = nil,
        totalBytes: UInt64? = nil,
        receivedBytes: UInt64 = 0,
        state: DownloadState,
        localURL: URL? = nil
    ) {
        self.fileId = fileId
        self.title = title
        self.subtitle = subtitle
        self.totalBytes = totalBytes
        self.receivedBytes = receivedBytes
        self.state = state
        self.localURL = localURL
    }
}
