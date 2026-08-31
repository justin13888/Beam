import AudioToolbox
import BeamFFI
import CoreMedia
import Foundation

/// The core's audio codec vocabulary.
///
/// Aliased because AudioToolbox exports an `AudioCodec` of its own -- an
/// opaque component type -- and an unqualified mention here resolves to
/// neither.
typealias CoreAudioCodec = BeamFFI.AudioCodec

/// Builds `CMFormatDescription`s from a container's codec-private bytes.
///
/// The demuxer deliberately hands over `CodecPrivate` verbatim and does not
/// interpret it: CoreMedia already knows how to read an `avcC`, and a second
/// parser in Rust would only find new ways to be wrong. What is left here is
/// the small amount of unpacking CoreMedia does *not* do -- pulling the
/// parameter sets out of the record, because
/// `CMVideoFormatDescriptionCreateFromH264ParameterSets` wants them
/// individually rather than as the record they arrived in.
enum SampleBufferFormats {
    /// Codecs this engine can build a video format description for.
    ///
    /// Narrower than what the demuxer can identify, and that gap is the point:
    /// a track whose codec is recognised but unbuildable is reported as
    /// unplayable with a reason, rather than as an unknown that silently
    /// produces no picture.
    static let supportedVideoCodecs: Set<VideoCodec> = [.h264, .h265]

    /// Codecs this engine can build an audio format description for.
    ///
    /// Opus, Vorbis, DTS and TrueHD are absent because Core Audio ships no
    /// decoder for them; `AVSampleBufferAudioRenderer` would accept the buffer
    /// and then produce silence.
    static let supportedAudioCodecs: Set<CoreAudioCodec> = [.aac, .ac3, .eac3, .flac, .mp3, .pcm]

    // MARK: - Video

    /// A format description for a video track, or `nil` when this engine
    /// cannot build one.
    static func videoFormat(for track: ExtractorTrack) -> CMFormatDescription? {
        guard let codec = track.videoCodec, supportedVideoCodecs.contains(codec) else {
            return nil
        }

        switch codec {
        case .h264:
            guard let sets = parameterSets(fromAVCC: track.codecPrivate) else { return nil }
            return makeVideoFormat(sets: sets, isHEVC: false)
        case .h265:
            guard let sets = parameterSets(fromHVCC: track.codecPrivate) else { return nil }
            return makeVideoFormat(sets: sets, isHEVC: true)
        default:
            return nil
        }
    }

    /// Parameter sets, plus the NAL length prefix width the record declares.
    struct ParameterSets {
        var sets: [Data]
        var nalUnitHeaderLength: Int32
    }

    /// Unpack an `avcC` record (ISO/IEC 14496-15 §5.3.3.1).
    ///
    /// Layout: version(1) profile(1) compat(1) level(1), then a byte whose low
    /// two bits are `lengthSizeMinusOne`, then a byte whose low five bits are
    /// the SPS count, then each SPS as a 16-bit length and its bytes, then a
    /// PPS count and the PPSs the same way.
    static func parameterSets(fromAVCC record: Data) -> ParameterSets? {
        var cursor = 0
        func byte() -> UInt8? {
            guard cursor < record.count else { return nil }
            defer { cursor += 1 }
            return record[record.startIndex + cursor]
        }
        func length() -> Int? {
            guard let high = byte(), let low = byte() else { return nil }
            return Int(high) << 8 | Int(low)
        }
        func bytes(_ count: Int) -> Data? {
            guard count >= 0, cursor + count <= record.count else { return nil }
            defer { cursor += count }
            let start = record.index(record.startIndex, offsetBy: cursor)
            return record[start..<record.index(start, offsetBy: count)]
        }

        guard let version = byte(), version == 1 else { return nil }
        _ = byte()  // profile
        _ = byte()  // profile compatibility
        _ = byte()  // level
        guard let lengthByte = byte() else { return nil }
        let nalLength = Int32((lengthByte & 0x03) + 1)

        guard let spsCountByte = byte() else { return nil }
        var sets: [Data] = []
        for _ in 0..<Int(spsCountByte & 0x1F) {
            guard let size = length(), let sps = bytes(size) else { return nil }
            sets.append(Data(sps))
        }
        guard let ppsCount = byte() else { return nil }
        for _ in 0..<Int(ppsCount) {
            guard let size = length(), let pps = bytes(size) else { return nil }
            sets.append(Data(pps))
        }

        guard !sets.isEmpty else { return nil }
        return ParameterSets(sets: sets, nalUnitHeaderLength: nalLength)
    }

    /// Unpack an `hvcC` record (ISO/IEC 14496-15 §8.3.3.1).
    ///
    /// Twenty-two fixed bytes, then `lengthSizeMinusOne` in the low two bits of
    /// byte 21, then an array count and that many arrays, each with a NAL type
    /// byte, a 16-bit count, and that many length-prefixed NAL units. VPS, SPS
    /// and PPS all arrive this way and are passed on in the order they appear,
    /// which is the order CoreMedia expects.
    static func parameterSets(fromHVCC record: Data) -> ParameterSets? {
        guard record.count > 23 else { return nil }
        let base = record.startIndex
        guard record[base] == 1 else { return nil }

        let nalLength = Int32((record[base + 21] & 0x03) + 1)
        var cursor = 23
        let arrayCount = Int(record[base + 22])
        var sets: [Data] = []

        for _ in 0..<arrayCount {
            // Skip the array header's NAL-type byte.
            guard cursor + 3 <= record.count else { return nil }
            cursor += 1
            let count = Int(record[base + cursor]) << 8 | Int(record[base + cursor + 1])
            cursor += 2

            for _ in 0..<count {
                guard cursor + 2 <= record.count else { return nil }
                let size = Int(record[base + cursor]) << 8 | Int(record[base + cursor + 1])
                cursor += 2
                guard cursor + size <= record.count else { return nil }
                let start = record.index(base, offsetBy: cursor)
                sets.append(Data(record[start..<record.index(start, offsetBy: size)]))
                cursor += size
            }
        }

        guard !sets.isEmpty else { return nil }
        return ParameterSets(sets: sets, nalUnitHeaderLength: nalLength)
    }

    private static func makeVideoFormat(sets: ParameterSets, isHEVC: Bool) -> CMFormatDescription? {
        // The pointers must stay valid across the call, so the parameter sets
        // are held for its whole duration rather than produced inline.
        var pointers: [UnsafePointer<UInt8>] = []
        var sizes: [Int] = []
        var buffers: [UnsafeMutableBufferPointer<UInt8>] = []
        defer { buffers.forEach { $0.deallocate() } }

        for set in sets.sets {
            let buffer = UnsafeMutableBufferPointer<UInt8>.allocate(capacity: set.count)
            _ = buffer.initialize(from: set)
            buffers.append(buffer)
            guard let base = buffer.baseAddress else { return nil }
            pointers.append(UnsafePointer(base))
            sizes.append(set.count)
        }

        var format: CMFormatDescription?
        let status =
            isHEVC
            ? CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                allocator: kCFAllocatorDefault,
                parameterSetCount: pointers.count,
                parameterSetPointers: &pointers,
                parameterSetSizes: &sizes,
                nalUnitHeaderLength: sets.nalUnitHeaderLength,
                extensions: nil,
                formatDescriptionOut: &format
            )
            : CMVideoFormatDescriptionCreateFromH264ParameterSets(
                allocator: kCFAllocatorDefault,
                parameterSetCount: pointers.count,
                parameterSetPointers: &pointers,
                parameterSetSizes: &sizes,
                nalUnitHeaderLength: sets.nalUnitHeaderLength,
                formatDescriptionOut: &format
            )

        return status == noErr ? format : nil
    }

    // MARK: - Audio

    /// A format description for an audio track, or `nil` when this engine
    /// cannot build one.
    static func audioFormat(for track: ExtractorTrack) -> CMFormatDescription? {
        guard let codec = track.audioCodec, supportedAudioCodecs.contains(codec),
            let formatID = audioFormatID(for: codec), track.sampleRate > 0
        else {
            return nil
        }

        var description = AudioStreamBasicDescription(
            mSampleRate: Double(track.sampleRate),
            mFormatID: formatID,
            mFormatFlags: 0,
            mBytesPerPacket: 0,
            mFramesPerPacket: framesPerPacket(for: codec),
            mBytesPerFrame: 0,
            mChannelsPerFrame: UInt32(max(track.channels, 1)),
            mBitsPerChannel: 0,
            mReserved: 0
        )

        // Linear PCM is the one format whose description must be complete:
        // there is no magic cookie to fill in the gaps, and a zeroed
        // `mBytesPerFrame` would have the renderer read frames of no length.
        if formatID == kAudioFormatLinearPCM {
            description.mFormatFlags = kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked
            description.mBitsPerChannel = 16
            description.mBytesPerFrame = 2 * description.mChannelsPerFrame
            description.mFramesPerPacket = 1
            description.mBytesPerPacket = description.mBytesPerFrame
        }

        var format: CMFormatDescription?
        let cookie = track.codecPrivate
        let status = cookie.withUnsafeBytes { raw -> OSStatus in
            CMAudioFormatDescriptionCreate(
                allocator: kCFAllocatorDefault,
                asbd: &description,
                layoutSize: 0,
                layout: nil,
                // AAC needs its AudioSpecificConfig as a magic cookie or the
                // decoder cannot be configured; AC-3 and E-AC-3 carry no
                // codec-private bytes in Matroska and need none.
                magicCookieSize: raw.count,
                magicCookie: raw.count > 0 ? raw.baseAddress : nil,
                extensions: nil,
                formatDescriptionOut: &format
            )
        }
        return status == noErr ? format : nil
    }

    private static func audioFormatID(for codec: CoreAudioCodec) -> AudioFormatID? {
        switch codec {
        case .aac: kAudioFormatMPEG4AAC
        case .ac3: kAudioFormatAC3
        case .eac3: kAudioFormatEnhancedAC3
        case .flac: kAudioFormatFLAC
        case .mp3: kAudioFormatMPEGLayer3
        case .pcm: kAudioFormatLinearPCM
        default: nil
        }
    }

    /// Frames in one packet, where the format has a fixed figure.
    ///
    /// The renderer needs this to convert a packet count into a duration; a
    /// zero here makes every sample's duration indeterminate and the
    /// synchronizer cannot then keep audio and video together.
    private static func framesPerPacket(for codec: CoreAudioCodec) -> UInt32 {
        switch codec {
        case .aac: 1024
        case .ac3: 1536
        case .eac3: 1536
        case .mp3: 1152
        case .pcm: 1
        default: 0
        }
    }
}
