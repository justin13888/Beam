import BeamFFI
import Foundation

/// Ranged HTTP reads for the demuxer.
///
/// The core parses the container but never fetches its bytes: the platform
/// already has a tuned HTTP stack with connection reuse and the user's trust
/// decisions wired in, and routing a whole media file through the FFI boundary
/// would buy nothing but copies. This is the Apple half of that split.
///
/// **These reads block, deliberately and by contract.** The Matroska parser
/// pulls bytes as it walks the element tree, so an async boundary would mean
/// either blocking on a future inside a sync parser or rewriting the parser.
/// `SampleBufferEngine` therefore drives the extractor from its own dispatch
/// queue and never from the main actor; calling this from the main thread
/// would deadlock the UI, which is why nothing above `BeamPlayback` is handed
/// one of these.
public final class HTTPByteSource: ByteSource, @unchecked Sendable {
    private let url: URL
    private let headers: [String: String]
    private let session: URLSession
    private let delegate: TrustingSessionDelegate
    private let totalLength: UInt64

    /// Open `url`, discovering its length up front.
    ///
    /// - Throws: ``ByteSourceError`` when the server will not say how long the
    ///   resource is. A demuxer needs a length to seek, and guessing one would
    ///   turn every seek past the guess into a corrupt-file report.
    public init(url: URL, headers: [String: String], evaluator: CertificateTrustEvaluator) throws {
        self.url = url
        self.headers = headers
        self.delegate = TrustingSessionDelegate(evaluator: evaluator)

        let configuration = URLSessionConfiguration.ephemeral
        // Beam sets `Cache-Control: public, max-age=3600` on stream responses,
        // and URLCache would happily hold a chunk of a film in memory. The
        // demuxer's own window is the only cache that should exist here.
        configuration.urlCache = nil
        configuration.requestCachePolicy = .reloadIgnoringLocalCacheData
        configuration.httpShouldSetCookies = false
        self.session = URLSession(
            configuration: configuration,
            delegate: delegate,
            delegateQueue: nil
        )

        self.totalLength = try Self.probeLength(
            url: url,
            headers: headers,
            session: session
        )
    }

    public func len() -> UInt64 {
        totalLength
    }

    public func readAt(offset: UInt64, length: UInt32) throws -> Data {
        guard length > 0 else { return Data() }
        let end = offset + UInt64(length) - 1
        guard end < totalLength else {
            throw ByteSourceError.OutOfBounds(offset: offset, length: UInt64(length))
        }

        var request = URLRequest(url: url)
        for (name, value) in headers {
            request.setValue(value, forHTTPHeaderField: name)
        }
        request.setValue("bytes=\(offset)-\(end)", forHTTPHeaderField: "Range")

        let (data, response) = try perform(request)
        guard let http = response as? HTTPURLResponse else {
            throw ByteSourceError.Unavailable(detail: "no HTTP response")
        }
        guard http.statusCode == 206 || http.statusCode == 200 else {
            throw ByteSourceError.Unavailable(detail: "HTTP \(http.statusCode)")
        }
        // A short read is a failure, not a truncated success: the parser reads
        // the returned slice as the bytes it asked for, and quietly returning
        // fewer would be reported as a malformed container rather than as a
        // failed fetch.
        guard data.count == Int(length) else {
            throw ByteSourceError.Unavailable(
                detail: "short read: wanted \(length), got \(data.count)"
            )
        }
        return data
    }

    // MARK: - Internals

    private func perform(_ request: URLRequest) throws -> (Data, URLResponse?) {
        try Self.perform(request, session: session)
    }

    private static func perform(
        _ request: URLRequest, session: URLSession
    ) throws -> (
        Data, URLResponse?
    ) {
        let semaphore = DispatchSemaphore(value: 0)
        // A box rather than captured vars: the completion runs on URLSession's
        // own queue, and this is the narrowest thing that can carry the result
        // back across it.
        final class Box: @unchecked Sendable {
            var data: Data?
            var response: URLResponse?
            var error: Error?
        }
        let box = Box()

        let task = session.dataTask(with: request) { data, response, error in
            box.data = data
            box.response = response
            box.error = error
            semaphore.signal()
        }
        task.resume()
        semaphore.wait()

        if let error = box.error {
            throw ByteSourceError.Unavailable(detail: error.localizedDescription)
        }
        return (box.data ?? Data(), box.response)
    }

    /// Ask for one byte and read the total out of `Content-Range`.
    ///
    /// A `HEAD` would be tidier, but `beam-server` declares only `GET` on the
    /// stream route, so a one-byte range is the request that is certain to be
    /// answered. The response also proves the endpoint is range-capable before
    /// the demuxer relies on it.
    private static func probeLength(
        url: URL,
        headers: [String: String],
        session: URLSession
    ) throws -> UInt64 {
        var request = URLRequest(url: url)
        for (name, value) in headers {
            request.setValue(value, forHTTPHeaderField: name)
        }
        request.setValue("bytes=0-0", forHTTPHeaderField: "Range")

        let (_, response) = try perform(request, session: session)
        guard let http = response as? HTTPURLResponse else {
            throw ByteSourceError.Unavailable(detail: "no HTTP response")
        }
        guard http.statusCode == 206,
            let contentRange = http.value(forHTTPHeaderField: "Content-Range"),
            let total = Self.total(fromContentRange: contentRange)
        else {
            throw ByteSourceError.Unavailable(
                detail: "server did not answer a range request with a length"
            )
        }
        return total
    }

    /// The total size out of a `Content-Range: bytes 0-0/12345` header.
    static func total(fromContentRange header: String) -> UInt64? {
        guard let slash = header.lastIndex(of: "/") else { return nil }
        let total = header[header.index(after: slash)...].trimmingCharacters(in: .whitespaces)
        // "*" is legal and means the server will not say. A demuxer cannot
        // work with that, so it is refused rather than guessed at.
        return UInt64(total)
    }
}

/// Reads for a file already on disk, for offline playback.
///
/// The same `ByteSource` the network path uses, so the extractor cannot tell
/// the difference and offline playback exercises exactly the code online
/// playback does.
public final class FileByteSource: ByteSource, @unchecked Sendable {
    private let handle: FileHandle
    private let totalLength: UInt64
    private let lock = NSLock()

    /// Open the file at `url`.
    public init(url: URL) throws {
        do {
            self.handle = try FileHandle(forReadingFrom: url)
            let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
            self.totalLength = (attributes[.size] as? NSNumber)?.uint64Value ?? 0
        } catch {
            throw ByteSourceError.Unavailable(detail: error.localizedDescription)
        }
    }

    deinit {
        try? handle.close()
    }

    public func len() -> UInt64 {
        totalLength
    }

    public func readAt(offset: UInt64, length: UInt32) throws -> Data {
        guard length > 0 else { return Data() }
        guard offset + UInt64(length) <= totalLength else {
            throw ByteSourceError.OutOfBounds(offset: offset, length: UInt64(length))
        }

        // One handle with a cursor, so concurrent reads would interleave a
        // seek with another read's offset. The extractor is single-threaded
        // today; the lock is what stops that being a silent assumption.
        lock.lock()
        defer { lock.unlock() }

        do {
            try handle.seek(toOffset: offset)
            guard let data = try handle.read(upToCount: Int(length)), data.count == Int(length)
            else {
                throw ByteSourceError.Unavailable(detail: "short read at \(offset)")
            }
            return data
        } catch let error as ByteSourceError {
            throw error
        } catch {
            throw ByteSourceError.Unavailable(detail: error.localizedDescription)
        }
    }
}
