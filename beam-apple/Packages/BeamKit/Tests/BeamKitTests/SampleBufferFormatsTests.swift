import BeamFFI
import CoreMedia
import Foundation
import Testing

@testable import BeamPlayback

/// Building decoder configuration out of a container's codec-private bytes.
///
/// Driven by the same real Matroska files `beam-client-core`'s demuxer tests
/// use, reached by path rather than copied a third time. A hand-built `avcC`
/// would agree with this parser by construction and could never catch the
/// case that matters: a record a real muxer wrote and this parser misreads.
@Suite("Sample buffer formats")
struct SampleBufferFormatsTests {
    /// `beam-client-core/fixtures`, found by walking up from this file.
    ///
    /// Not a SwiftPM resource: copying the bytes into this target would be a
    /// fourth copy of the same four files and a fourth thing to regenerate.
    /// See that directory's README.
    ///
    /// Searched for rather than reached by a fixed number of `..` steps, so
    /// moving this file up or down a directory does not silently produce a
    /// path to nothing -- which fails as "the fixture does not exist" rather
    /// than as anything that points at the real cause.
    static var fixturesDirectory: URL {
        var directory = URL(filePath: #filePath).deletingLastPathComponent()
        for _ in 0..<8 {
            let candidate = directory.appending(path: "beam-client-core/fixtures")
            if FileManager.default.fileExists(atPath: candidate.path) {
                return candidate
            }
            directory = directory.deletingLastPathComponent()
        }
        return URL(filePath: #filePath).deletingLastPathComponent()
    }

    static func tracks(in fixture: String) throws -> [ExtractorTrack] {
        let url = fixturesDirectory.appending(path: fixture)
        let source = try FileByteSource(url: url)
        return try MatroskaExtractor.open(source: source).tracks()
    }

    @Test("an H.264 track yields a format description with the file's dimensions")
    func h264FormatDescription() throws {
        let tracks = try Self.tracks(in: "h264.mkv")
        let video = try #require(tracks.first { $0.kind == .video })

        let format = try #require(SampleBufferFormats.videoFormat(for: video))
        let dimensions = CMVideoFormatDescriptionGetDimensions(format)

        #expect(CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_H264)
        #expect(dimensions.width == 64)
        #expect(dimensions.height == 64)
    }

    @Test("an HEVC track yields a format description")
    func hevcFormatDescription() throws {
        // A different CodecPrivate shape entirely -- hvcC is an array of NAL
        // arrays where avcC is two flat lists -- so this exercises a wholly
        // separate parser, not the same one twice.
        let tracks = try Self.tracks(in: "hevc.mkv")
        let video = try #require(tracks.first { $0.kind == .video })

        let format = try #require(SampleBufferFormats.videoFormat(for: video))

        #expect(CMFormatDescriptionGetMediaSubType(format) == kCMVideoCodecType_HEVC)
    }

    @Test("an AAC track yields an audio format description carrying its magic cookie")
    func aacFormatDescription() throws {
        let tracks = try Self.tracks(in: "h264_aac.mkv")
        let audio = try #require(tracks.first { $0.kind == .audio })

        let format = try #require(SampleBufferFormats.audioFormat(for: audio))
        let asbd = try #require(CMAudioFormatDescriptionGetStreamBasicDescription(format)?.pointee)

        #expect(asbd.mFormatID == kAudioFormatMPEG4AAC)
        #expect(asbd.mSampleRate == 44_100)
        // Without a frames-per-packet figure the renderer cannot turn a packet
        // count into a duration, and the synchronizer cannot then keep audio
        // and video together.
        #expect(asbd.mFramesPerPacket == 1024)
    }

    @Test("a codec with no Apple decoder yields no format description")
    func opusIsRefused() throws {
        // Reported rather than silently accepted: an AVSampleBufferAudioRenderer
        // handed an Opus buffer produces silence, which is far worse to debug
        // than a track marked unsupported in the menu.
        let tracks = try Self.tracks(in: "vp9_opus.webm")
        let audio = try #require(tracks.first { $0.kind == .audio })

        #expect(audio.audioCodec == .opus)
        #expect(SampleBufferFormats.audioFormat(for: audio) == nil)
    }

    @Test("a video codec with no VideoToolbox path yields no format description")
    func vp9IsRefused() throws {
        // VP9 has a hardware decoder on recent hardware but no parameter-set
        // constructor: there is no CMVideoFormatDescriptionCreateFromVP9...,
        // so this engine cannot configure one. Saying so is what lets the
        // source picker offer a reason.
        let tracks = try Self.tracks(in: "vp9_opus.webm")
        let video = try #require(tracks.first { $0.kind == .video })

        #expect(video.videoCodec == .vp9)
        #expect(SampleBufferFormats.videoFormat(for: video) == nil)
    }

    @Test("a truncated avcC record is refused rather than half-read")
    func truncatedAVCC() throws {
        let tracks = try Self.tracks(in: "h264.mkv")
        let video = try #require(tracks.first { $0.kind == .video })
        let full = video.codecPrivate
        #expect(!full.isEmpty)

        for length in 0..<full.count {
            let truncated = full.prefix(length)
            let sets = SampleBufferFormats.parameterSets(fromAVCC: Data(truncated))
            // Any prefix short of the whole record must be refused: a parser
            // that read past its input would hand VideoToolbox parameter sets
            // made of whatever followed in memory.
            if let sets {
                #expect(
                    sets.sets.allSatisfy { !$0.isEmpty },
                    "a \(length)-byte prefix produced an empty parameter set"
                )
            }
        }
    }
}
