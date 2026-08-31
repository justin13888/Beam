import AVFoundation
import BeamFFI
import BeamModel
import CoreMedia
import Foundation
import SwiftUI

/// The engine for containers AVFoundation cannot open.
///
/// This exists for one reason. `beam-server` never remuxes (ADR-0004), so an
/// `.mkv` arrives as an `.mkv`, and AVFoundation cannot demux Matroska at all
/// -- not badly, not partially. Every other option was worse: shipping a
/// GPL-licensed player, or telling a large share of a self-hosted library that
/// it will never play.
///
/// What it does **not** do is as important as what it does. It decodes
/// nothing. The core demuxes the container into encoded samples, this wraps
/// them in `CMSampleBuffer`s, and VideoToolbox and Core Audio decode them in
/// hardware exactly as they would for an MP4. Owning the container parsing and
/// nothing else is what keeps the hardware decode path -- which is the entire
/// value proposition of a native client (ADR-0012).
///
/// The costs, stated rather than discovered: no Picture in Picture, no AirPlay
/// and no system transport bar, because those are `AVPlayer` features. That is
/// why `EngineSelector` prefers `AVPlayerEngine` wherever it can open the file.
@MainActor
public final class SampleBufferEngine: PlaybackEngine {
    public private(set) var snapshot = PlaybackSnapshot()
    public var onSnapshotChange: (@MainActor (PlaybackSnapshot) -> Void)?

    private let synchronizer = AVSampleBufferRenderSynchronizer()
    // The renderer belongs to the display layer rather than standing alone:
    // `AVSampleBufferVideoRenderer` is how a layer's pipeline is addressed, so
    // the layer has to exist first and the same layer has to be the one the
    // SwiftUI view hosts. Creating a renderer separately would enqueue frames
    // into a pipeline nothing is displaying.
    private let displayLayer = AVSampleBufferDisplayLayer()
    private var videoRenderer: AVSampleBufferVideoRenderer { displayLayer.sampleBufferRenderer }
    private var audioRenderer: AVSampleBufferAudioRenderer?

    private let demuxQueue = DispatchQueue(label: "net.justinchung.beam.demux", qos: .userInitiated)
    private var pump: DemuxPump?
    private var tracks: [ExtractorTrack] = []
    private var videoTrack: ExtractorTrack?
    private var audioTrack: ExtractorTrack?
    private var subtitleTrack: ExtractorTrack?
    private var videoFormat: CMFormatDescription?
    private var audioFormat: CMFormatDescription?
    private var subtitleCues: [SubtitleCue] = []
    private var timeObserver: Any?
    private var byteSource: ByteSource?
    private var loadedExtractor: MatroskaExtractor?

    /// An engine with nothing loaded.
    public init() {
        displayLayer.videoGravity = .resizeAspect
        synchronizer.addRenderer(videoRenderer)
    }

    public func makeVideoView() -> AnyView {
        AnyView(SampleBufferVideoView(displayLayer: displayLayer))
    }

    public func load(_ item: PlaybackItem) async throws {
        stop()
        update { $0.status = .loading }

        let source = try makeByteSource(for: item)
        byteSource = source

        // Opening reads the header, which is network I/O on a blocking byte
        // source. Off the main actor, always.
        let extractor: MatroskaExtractor = try await withCheckedThrowingContinuation {
            continuation in
            demuxQueue.async {
                do {
                    continuation.resume(returning: try MatroskaExtractor.open(source: source))
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }

        let allTracks = extractor.tracks()
        tracks = allTracks
        videoTrack = Self.bestVideoTrack(in: allTracks)
        audioTrack = Self.bestAudioTrack(in: allTracks, preferring: nil)
        subtitleTrack = nil

        guard let videoTrack, let format = SampleBufferFormats.videoFormat(for: videoTrack) else {
            throw PlaybackEngineError.unsupportedVideo(
                detail: Self.unsupportedVideoDetail(for: allTracks)
            )
        }
        videoFormat = format

        if let audioTrack {
            audioFormat = SampleBufferFormats.audioFormat(for: audioTrack)
            if audioFormat != nil {
                let renderer = AVSampleBufferAudioRenderer()
                synchronizer.addRenderer(renderer)
                audioRenderer = renderer
            }
        }

        let pump = DemuxPump(extractor: extractor)
        self.pump = pump
        loadedExtractor = extractor
        extractor.selectTracks(tracks: selectedTrackNumbers())

        update {
            $0.duration = extractor.durationSeconds()
            $0.audioTracks = Self.playbackTracks(from: allTracks, kind: .audio)
            $0.subtitleTracks = Self.playbackTracks(from: allTracks, kind: .subtitle)
            $0.selectedAudioTrackID = self.audioTrack.map { String($0.number) }
            $0.isSeekable = true
            $0.status = .paused
        }

        if item.startPositionSeconds > 0 {
            await seek(to: item.startPositionSeconds)
        }
        startFeeding()
        observeTime()
    }

    public func play() {
        synchronizer.rate = 1
        update { $0.status = .playing }
    }

    public func pause() {
        synchronizer.rate = 0
        update { $0.status = .paused }
    }

    public func seek(to seconds: Double) async {
        guard let pump else { return }

        synchronizer.rate = 0
        videoRenderer.flush()
        audioRenderer?.flush()

        let landed: Double? = await withCheckedContinuation { continuation in
            demuxQueue.async {
                continuation.resume(returning: try? pump.seek(to: seconds))
            }
        }

        // The extractor lands on the keyframe at or before the request, so the
        // timeline is set to where it actually landed rather than to what was
        // asked for. Setting it to the request would make every subsequent
        // position report drift by the gap between them.
        let position = landed ?? seconds
        synchronizer.setRate(0, time: CMTime(seconds: position, preferredTimescale: 600))
        update { $0.position = position }
    }

    public func selectAudioTrack(id: String) {
        guard let number = UInt64(id),
            let track = tracks.first(where: { $0.number == number }),
            let format = SampleBufferFormats.audioFormat(for: track)
        else {
            return
        }

        audioTrack = track
        audioFormat = format
        if let existing = audioRenderer {
            synchronizer.removeRenderer(existing, at: synchronizer.currentTime())
        }
        let renderer = AVSampleBufferAudioRenderer()
        synchronizer.addRenderer(renderer)
        audioRenderer = renderer

        pump.map { _ in }
        update { $0.selectedAudioTrackID = id }

        // Re-seek to the current position so the new track starts in step with
        // the video rather than wherever in the file the demuxer had reached.
        let position = snapshot.position
        Task { await seek(to: position) }
    }

    public func selectSubtitleTrack(id: String?) {
        guard let id, let number = UInt64(id),
            let track = tracks.first(where: { $0.number == number })
        else {
            subtitleTrack = nil
            subtitleCues = []
            update {
                $0.selectedSubtitleTrackID = nil
                $0.activeSubtitleText = nil
            }
            return
        }

        subtitleTrack = track
        subtitleCues = []
        pump.map { $0.reset(track: number) }
        update { $0.selectedSubtitleTrackID = id }

        // Track selection is the extractor's, not just ours: an unselected
        // track yields no samples at all, so a subtitle chosen mid-playback
        // has to be re-selected there before any cue can arrive.
        Task { await self.reselectExtractorTracks() }
    }

    /// Re-apply the track selection and resume from the current position.
    private func reselectExtractorTracks() async {
        guard let extractor = loadedExtractor else { return }
        extractor.selectTracks(tracks: selectedTrackNumbers())
        await seek(to: snapshot.position)
    }

    public func stop() {
        if let timeObserver {
            synchronizer.removeTimeObserver(timeObserver)
            self.timeObserver = nil
        }
        synchronizer.rate = 0
        videoRenderer.stopRequestingMediaData()
        videoRenderer.flush()
        if let audioRenderer {
            audioRenderer.stopRequestingMediaData()
            audioRenderer.flush()
            synchronizer.removeRenderer(audioRenderer, at: synchronizer.currentTime())
        }
        audioRenderer = nil
        pump = nil
        loadedExtractor = nil
        byteSource = nil
        tracks = []
        videoTrack = nil
        audioTrack = nil
        subtitleTrack = nil
        videoFormat = nil
        audioFormat = nil
        subtitleCues = []
    }

    // MARK: - Feeding

    private func startFeeding() {
        guard let pump, let videoTrack, let videoFormat else { return }

        let videoNumber = videoTrack.number
        let renderer = RendererBox(videoRenderer)
        renderer.value.requestMediaDataWhenReady(on: demuxQueue) {
            while renderer.value.isReadyForMoreMediaData {
                guard let sample = try? pump.next(track: videoNumber) else {
                    // No sample means end of file or starvation. Stop asking
                    // until the next callback; continuing would spin the queue
                    // at full tilt for nothing.
                    return
                }
                guard let buffer = Self.makeSampleBuffer(from: sample, format: videoFormat) else {
                    continue
                }
                renderer.value.enqueue(buffer)
            }
        }

        if let audioRenderer, let audioFormat, let audioTrack {
            let audioNumber = audioTrack.number
            let renderer = RendererBox(audioRenderer)
            renderer.value.requestMediaDataWhenReady(on: demuxQueue) {
                while renderer.value.isReadyForMoreMediaData {
                    guard let sample = try? pump.next(track: audioNumber) else { return }
                    guard let buffer = Self.makeSampleBuffer(from: sample, format: audioFormat)
                    else {
                        continue
                    }
                    renderer.value.enqueue(buffer)
                }
            }
        }
    }

    private func observeTime() {
        timeObserver = synchronizer.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 0.25, preferredTimescale: 600),
            queue: .main
        ) { [weak self] time in
            Task { @MainActor [weak self] in
                guard let self else { return }
                let seconds = time.seconds
                self.drainSubtitles()
                self.update {
                    $0.position = seconds
                    $0.activeSubtitleText = self.subtitleCues.first { $0.contains(seconds) }?.text
                    if let duration = $0.duration, seconds >= duration - 0.25,
                        self.pump?.isAtEnd == true
                    {
                        $0.status = .ended
                    }
                }
            }
        }
    }

    /// Turn whatever subtitle samples the pump has buffered into cues.
    ///
    /// Subtitles are pulled here rather than through their own
    /// `requestMediaDataWhenReady` loop because there is no renderer for them
    /// -- text cues are drawn by SwiftUI over the video. The pump buffers them
    /// as a side effect of feeding video and audio, so this only has to take
    /// what has already arrived and never blocks on a read.
    ///
    /// Cues are kept only around the current position: a three-hour film with
    /// styled subtitles is tens of thousands of lines, and holding them all
    /// would be a steadily growing array for no benefit.
    private func drainSubtitles() {
        guard let pump, let subtitleTrack else { return }
        let format = subtitleTrack.subtitleFormat
        var added: [SubtitleCue] = []
        while let sample = (try? pump.buffered(track: subtitleTrack.number)) ?? nil {
            if let cue = SubtitleCue.from(sample: sample, format: format) {
                added.append(cue)
            }
        }
        guard !added.isEmpty else { return }

        let position = snapshot.position
        subtitleCues.append(contentsOf: added)
        subtitleCues.removeAll { $0.end < position - 5 }
    }

    private func selectedTrackNumbers() -> [UInt64] {
        [videoTrack?.number, audioTrack?.number, subtitleTrack?.number].compactMap { $0 }
    }

    private func makeByteSource(for item: PlaybackItem) throws -> ByteSource {
        if item.url.isFileURL {
            return try FileByteSource(url: item.url)
        }
        return try HTTPByteSource(
            url: item.url,
            headers: item.headers,
            evaluator: CertificateTrustEvaluator(
                trustedFingerprints: item.trustedFingerprints,
                pinnedHost: item.pinnedHost
            )
        )
    }

    private func update(_ mutate: (inout PlaybackSnapshot) -> Void) {
        mutate(&snapshot)
        onSnapshotChange?(snapshot)
    }

    // MARK: - Track choice and buffer construction

    nonisolated static func bestVideoTrack(in tracks: [ExtractorTrack]) -> ExtractorTrack? {
        let video = tracks.filter { $0.kind == .video }
        // A default-flagged track wins, then the first: Matroska permits
        // several video tracks and the flag is the muxer telling us which one
        // is the film rather than, say, a thumbnail track.
        return video.first(where: \.isDefault) ?? video.first
    }

    nonisolated static func bestAudioTrack(
        in tracks: [ExtractorTrack],
        preferring languages: [String]?
    ) -> ExtractorTrack? {
        let playable = tracks.filter { track in
            track.kind == .audio && SampleBufferFormats.audioFormat(for: track) != nil
        }
        if let languages {
            for language in languages {
                if let match = playable.first(where: { $0.language?.hasPrefix(language) == true }) {
                    return match
                }
            }
        }
        return playable.first(where: \.isDefault) ?? playable.first
    }

    nonisolated static func playbackTracks(
        from tracks: [ExtractorTrack], kind: TrackKind
    ) -> [PlaybackTrack] {
        tracks.filter { $0.kind == kind }.map { track in
            PlaybackTrack(
                id: String(track.number),
                label: Self.label(for: track),
                languageCode: track.language,
                isDefault: track.isDefault,
                isPlayable: Self.isPlayable(track)
            )
        }
    }

    /// Whether this engine can actually render a track.
    ///
    /// Reported rather than filtered, for the same reason `capability::select`
    /// returns rejected sources with their reason: "this track has no decoder
    /// on Apple platforms" is a permanent fact worth telling someone, and a
    /// menu that silently omits the DTS track looks like a bug.
    nonisolated static func isPlayable(_ track: ExtractorTrack) -> Bool {
        switch track.kind {
        case .audio: SampleBufferFormats.audioFormat(for: track) != nil
        case .subtitle: SubtitleCue.isRenderable(track)
        case .video: SampleBufferFormats.videoFormat(for: track) != nil
        case .other: false
        }
    }

    nonisolated static func label(for track: ExtractorTrack) -> String {
        var parts: [String] = []
        if let name = track.name, !name.isEmpty {
            parts.append(name)
        } else if let language = track.language,
            let localized = Locale.current.localizedString(forLanguageCode: language)
        {
            parts.append(localized)
        } else {
            parts.append("Track \(track.number)")
        }

        if track.kind == .audio, track.channels > 0 {
            parts.append(channelLayout(track.channels))
        }
        if track.isForced {
            parts.append("Forced")
        }
        if !isPlayable(track) {
            parts.append("unsupported")
        }
        return parts.joined(separator: " - ")
    }

    private nonisolated static func channelLayout(_ channels: UInt16) -> String {
        switch channels {
        case 1: "Mono"
        case 2: "Stereo"
        case 6: "5.1"
        case 8: "7.1"
        default: "\(channels) ch"
        }
    }

    /// A detail explaining why no video track could be played.
    nonisolated static func unsupportedVideoDetail(for tracks: [ExtractorTrack]) -> String {
        guard let video = tracks.first(where: { $0.kind == .video }) else {
            return "This file has no video track."
        }
        let codec = video.codecId
        return "This device cannot play \(codec) in this container."
    }

    /// Wrap an encoded sample so a renderer can take it.
    ///
    /// Matroska stores AVC and HEVC in the length-prefixed form CoreMedia
    /// wants, so no Annex-B conversion is needed -- the bytes go across as
    /// they came out of the container.
    nonisolated static func makeSampleBuffer(
        from sample: EncodedSample,
        format: CMFormatDescription
    ) -> CMSampleBuffer? {
        var blockBuffer: CMBlockBuffer?
        var data = sample.data

        let status = data.withUnsafeMutableBytes { raw -> OSStatus in
            CMBlockBufferCreateWithMemoryBlock(
                allocator: kCFAllocatorDefault,
                memoryBlock: raw.baseAddress,
                blockLength: raw.count,
                // `kCFAllocatorNull` because the bytes belong to `data`, whose
                // lifetime ends with this call. They are copied immediately
                // below; handing ownership over would free memory the sample
                // buffer still points at.
                blockAllocator: kCFAllocatorNull,
                customBlockSource: nil,
                offsetToData: 0,
                dataLength: raw.count,
                flags: 0,
                blockBufferOut: &blockBuffer
            )
        }
        guard status == kCMBlockBufferNoErr, let blockBuffer else { return nil }

        var copied: CMBlockBuffer?
        guard
            CMBlockBufferCreateContiguous(
                allocator: kCFAllocatorDefault,
                sourceBuffer: blockBuffer,
                blockAllocator: kCFAllocatorDefault,
                customBlockSource: nil,
                offsetToData: 0,
                dataLength: 0,
                flags: kCMBlockBufferAlwaysCopyDataFlag,
                blockBufferOut: &copied
            ) == kCMBlockBufferNoErr, let copied
        else {
            return nil
        }

        var timing = CMSampleTimingInfo(
            duration: sample.durationSeconds.map {
                CMTime(seconds: $0, preferredTimescale: 600)
            } ?? .invalid,
            presentationTimeStamp: CMTime(seconds: sample.ptsSeconds, preferredTimescale: 600),
            // No decode timestamp: Matroska stores presentation order, and
            // declaring a DTS equal to the PTS would misreport B-frame
            // ordering. `.invalid` lets the decoder reorder for itself.
            decodeTimeStamp: .invalid
        )
        var size = sample.data.count

        var buffer: CMSampleBuffer?
        guard
            CMSampleBufferCreateReady(
                allocator: kCFAllocatorDefault,
                dataBuffer: copied,
                formatDescription: format,
                sampleCount: 1,
                sampleTimingEntryCount: 1,
                sampleTimingArray: &timing,
                sampleSizeEntryCount: 1,
                sampleSizeArray: &size,
                sampleBufferOut: &buffer
            ) == noErr, let buffer
        else {
            return nil
        }

        if !sample.isKeyframe {
            Self.markAsNonKeyframe(buffer)
        }
        return buffer
    }

    /// Tell the decoder this sample is not a random-access point.
    ///
    /// Without it the decoder may try to start on a P-frame after a flush and
    /// emit a corrupt picture instead of waiting for the next keyframe.
    private nonisolated static func markAsNonKeyframe(_ buffer: CMSampleBuffer) {
        guard
            let attachments = CMSampleBufferGetSampleAttachmentsArray(
                buffer,
                createIfNecessary: true
            )
        else {
            return
        }
        let first = unsafeBitCast(
            CFArrayGetValueAtIndex(attachments, 0),
            to: CFMutableDictionary.self
        )
        CFDictionarySetValue(
            first,
            Unmanaged.passUnretained(kCMSampleAttachmentKey_NotSync).toOpaque(),
            Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
        )
    }
}

/// Carries a renderer into its own `requestMediaDataWhenReady` callback.
///
/// The renderers are not `Sendable`, and the compiler is right to say so in
/// general. It is safe here for a specific documented reason:
/// `requestMediaDataWhenReady(on:using:)` serialises every invocation of the
/// block onto the queue it is given, and `enqueue` and `isReadyForMoreMediaData`
/// are the operations that block is for. The renderer is therefore touched from
/// exactly one queue, which is what `Sendable` would have been asserting.
///
/// The box exists so that assertion is written down once, next to its reason,
/// rather than as a `@preconcurrency import` that would silence every
/// concurrency diagnostic AVFoundation can produce.
private final class RendererBox<Renderer>: @unchecked Sendable {
    let value: Renderer

    init(_ value: Renderer) {
        self.value = value
    }
}

/// What can go wrong before a single frame is decoded.
public enum PlaybackEngineError: Error, Equatable {
    /// The container has no video track this engine can build a decoder for.
    case unsupportedVideo(detail: String)

    /// A message fit to show.
    public var message: String {
        switch self {
        case .unsupportedVideo(let detail): detail
        }
    }
}

/// Hosts the engine's `AVSampleBufferDisplayLayer` in SwiftUI.
///
/// The engine's layer, not a new one: the renderer frames are enqueued into
/// belongs to that specific layer, and hosting a different one would show
/// nothing while playback appeared to proceed.
private struct SampleBufferVideoView: View {
    let displayLayer: AVSampleBufferDisplayLayer

    var body: some View {
        #if os(iOS)
        IOSSampleBufferView(displayLayer: displayLayer).ignoresSafeArea()
        #else
        MacSampleBufferView(displayLayer: displayLayer)
        #endif
    }
}

#if os(iOS)
import UIKit

private struct IOSSampleBufferView: UIViewRepresentable {
    let displayLayer: AVSampleBufferDisplayLayer

    func makeUIView(context: Context) -> SampleBufferHostView {
        SampleBufferHostView(displayLayer: displayLayer)
    }

    func updateUIView(_ view: SampleBufferHostView, context: Context) {}
}

final class SampleBufferHostView: UIView {
    private let displayLayer: AVSampleBufferDisplayLayer

    init(displayLayer: AVSampleBufferDisplayLayer) {
        self.displayLayer = displayLayer
        super.init(frame: .zero)
        layer.addSublayer(displayLayer)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("not supported")
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        // No implicit animation: the layer is resized on rotation, and an
        // animated bounds change makes the picture visibly stretch first.
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        displayLayer.frame = bounds
        CATransaction.commit()
    }
}
#else
import AppKit

private struct MacSampleBufferView: NSViewRepresentable {
    let displayLayer: AVSampleBufferDisplayLayer

    func makeNSView(context: Context) -> SampleBufferHostView {
        SampleBufferHostView(displayLayer: displayLayer)
    }

    func updateNSView(_ view: SampleBufferHostView, context: Context) {}
}

final class SampleBufferHostView: NSView {
    private let displayLayer: AVSampleBufferDisplayLayer

    init(displayLayer: AVSampleBufferDisplayLayer) {
        self.displayLayer = displayLayer
        super.init(frame: .zero)
        wantsLayer = true
        layer?.addSublayer(displayLayer)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        fatalError("not supported")
    }

    override func layout() {
        super.layout()
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        displayLayer.frame = bounds
        CATransaction.commit()
    }
}
#endif
