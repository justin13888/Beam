import BeamFFI
import Foundation

/// Pulls samples from one extractor and hands them out per track.
///
/// Two renderers ask for data independently -- `AVSampleBufferVideoRenderer`
/// and `AVSampleBufferAudioRenderer` each drive their own
/// `requestMediaDataWhenReady` loop -- but there is only one extractor and it
/// yields whatever comes next in the file, in whatever order the muxer
/// interleaved it. So a read for video routinely produces an audio sample and
/// the other way round, and the sample that arrived for the wrong caller must
/// not be dropped.
///
/// This holds one queue per track and reads ahead until the requested track
/// has something. The read-ahead is bounded: a badly interleaved file (a whole
/// audio track written after the video) would otherwise pull the entire
/// remainder into memory looking for one video frame.
///
/// A lock rather than an actor, because both callers arrive on
/// `requestMediaDataWhenReady`'s dispatch queue synchronously and expect a
/// synchronous answer -- an actor hop would mean returning `nil` and waiting
/// for the next callback, which stalls the renderer it was meant to feed.
final class DemuxPump: @unchecked Sendable {
    /// How many samples for other tracks may be buffered before a read for one
    /// track gives up and reports starvation.
    private static let readAheadLimit = 512

    private let extractor: MatroskaExtractor
    private let lock = NSLock()
    private var queues: [UInt64: [EncodedSample]] = [:]
    private var reachedEnd = false
    private var failure: Error?

    init(extractor: MatroskaExtractor) {
        self.extractor = extractor
    }

    /// The next sample on `track`, or `nil` at end of file or on starvation.
    ///
    /// - Throws: the extractor's own error, once. A failed extractor is
    ///   remembered so every subsequent call fails the same way rather than
    ///   retrying a read that cannot succeed.
    func next(track: UInt64) throws -> EncodedSample? {
        lock.lock()
        defer { lock.unlock() }

        if let failure { throw failure }
        if let queued = dequeue(track) { return queued }
        if reachedEnd { return nil }

        var buffered = 0
        while buffered < Self.readAheadLimit {
            let sample: EncodedSample?
            do {
                sample = try extractor.nextSample()
            } catch {
                failure = error
                throw error
            }

            guard let sample else {
                reachedEnd = true
                return nil
            }
            if sample.track == track {
                return sample
            }
            queues[sample.track, default: []].append(sample)
            buffered += 1
        }

        // Starvation rather than end of file. Returning `nil` here pauses the
        // renderer that asked; the other renderer will drain its queue and the
        // next request will get further.
        return nil
    }

    /// A sample already buffered for `track`, without reading ahead.
    ///
    /// Distinct from ``next(track:)`` on purpose: subtitles have no renderer
    /// asking for them, so pulling them must never drive a read. Doing so
    /// would fetch bytes ahead of what the video needs and stall the picture
    /// to fill a subtitle queue.
    ///
    /// - Returns: the next buffered sample, or `nil` when none is buffered.
    func buffered(track: UInt64) throws -> EncodedSample? {
        lock.lock()
        defer { lock.unlock() }
        if let failure { throw failure }
        return dequeue(track)
    }

    /// Drop everything buffered for one track.
    ///
    /// Used when a subtitle track is chosen: whatever is queued belongs to the
    /// track that was showing, and rendering it under the new selection would
    /// show the old language for a few seconds.
    func reset(track: UInt64) {
        lock.lock()
        defer { lock.unlock() }
        queues[track] = []
    }

    /// Whether the file has been read to its end.
    var isAtEnd: Bool {
        lock.lock()
        defer { lock.unlock() }
        return reachedEnd && queues.values.allSatisfy(\.isEmpty)
    }

    /// Seek, discarding everything buffered.
    ///
    /// The buffers must go: they hold samples from before the seek, and
    /// feeding those to a renderer that has just been flushed would play a
    /// second of the old position before the new one arrived.
    func seek(to seconds: Double) throws -> Double {
        lock.lock()
        defer { lock.unlock() }
        queues.removeAll()
        reachedEnd = false
        failure = nil
        return try extractor.seek(seconds: seconds)
    }

    private func dequeue(_ track: UInt64) -> EncodedSample? {
        guard var queue = queues[track], !queue.isEmpty else { return nil }
        let sample = queue.removeFirst()
        queues[track] = queue
        return sample
    }
}
