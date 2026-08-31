import AVFoundation
import AudioToolbox
import BeamCoreBindings
import CoreMedia
import Foundation
import UniformTypeIdentifiers
import VideoToolbox

#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

/// Builds the `DeviceProfile` the core matches sources against.
///
/// This is the Apple counterpart of `DeviceProfiles.kt`, and it is the single
/// most consequential file in the client: under direct play ([ADR-0004]) the
/// core refuses any source this profile does not cover, so an omission here is
/// a title that silently will not play, and an overstatement is one that
/// starts and then fails.
///
/// [ADR-0004]: ADR-0004-never-transcode.md
public enum DeviceProfileFactory {
    /// Every video codec worth asking VideoToolbox about, as the
    /// `CMVideoCodecType` the core's `from_apple_fourcc` table expects.
    private static let videoCandidates: [(type: CMVideoCodecType, fourCC: String)] = [
        (kCMVideoCodecType_H264, "avc1"),
        (kCMVideoCodecType_HEVC, "hvc1"),
        (kCMVideoCodecType_AV1, "av01"),
        (kCMVideoCodecType_VP9, "vp09"),
        (kCMVideoCodecType_MPEG4Video, "mp4v"),
        (kCMVideoCodecType_MPEG2Video, "mp2v"),
    ]

    /// Audio formats Core Audio may have a decoder for, as `AudioFormatID`s.
    ///
    /// Opus, Vorbis, DTS and TrueHD are absent by construction: Core Audio
    /// ships no decoder for them, so `AVSampleBufferAudioRenderer` cannot play
    /// them and claiming otherwise would offer a source that fails on the
    /// first packet. The core still *recognises* those codecs, which is what
    /// lets the source picker say why a file will not play rather than
    /// reporting it as unknown.
    private static let audioCandidates: [(id: AudioFormatID, fourCC: String)] = [
        (kAudioFormatMPEG4AAC, "aac "),
        (kAudioFormatAC3, "ac-3"),
        (kAudioFormatEnhancedAC3, "ec-3"),
        (kAudioFormatFLAC, "flac"),
        (kAudioFormatMPEGLayer3, ".mp3"),
        (kAudioFormatLinearPCM, "lpcm"),
    ]

    /// Assemble a profile for this device.
    ///
    /// - Parameters:
    ///   - allowSoftwareDecode: the user's preference, passed through to the
    ///     core rather than acted on here -- the core owns the policy, and
    ///     this owns only the facts.
    ///   - display: the screen to size against. Injected so a test can pin it
    ///     rather than depending on whichever simulator is running.
    @MainActor
    public static func make(
        allowSoftwareDecode: Bool,
        display: DisplayCapabilities = .current()
    ) -> DeviceProfile {
        DeviceProfile(
            videoDecoders: videoDecoders(maxWidth: display.width, maxHeight: display.height),
            audioDecoders: audioDecoders(),
            supportedContainers: supportedContainers(),
            displayWidth: display.width,
            displayHeight: display.height,
            displaySupportsHdr: display.supportsHDR,
            preferredAudioLanguages: preferredAudioLanguages(),
            allowSoftwareDecode: allowSoftwareDecode
        )
    }

    /// The containers this client can open, from both engines.
    ///
    /// The union is the point. AVFoundation contributes what `AVPlayerEngine`
    /// can open; the core's own extractor contributes Matroska and WebM, which
    /// AVFoundation cannot. Reading the second half from
    /// `probeContainers()` rather than hardcoding it means the client cannot
    /// claim a container the demuxer was never taught.
    public static func supportedContainers() -> [String] {
        var containers = Set(probeContainers())

        for type in AVURLAsset.audiovisualTypes() {
            guard let utType = UTType(type.rawValue) else { continue }
            containers.formUnion(utType.tags[.filenameExtension] ?? [])
        }

        // AVFoundation names these by UTI, and the extensions it reports do
        // not always include the spelling a library actually uses.
        containers.formUnion(["mp4", "m4v", "mov"])
        return containers.sorted()
    }

    private static func videoDecoders(maxWidth: UInt32, maxHeight: UInt32) -> [DecoderCapability] {
        videoCandidates.compactMap { candidate in
            // `VTIsHardwareDecodeSupported` answers only for hardware. A codec
            // it refuses may still have a software decoder, and reporting that
            // is what lets the core offer it under the user's software-decode
            // preference instead of hiding the source entirely.
            let hardware = VTIsHardwareDecodeSupported(candidate.type)
            guard hardware || softwareDecodable.contains(candidate.fourCC) else { return nil }

            return DecoderCapability(
                mimeType: candidate.fourCC,
                isHardwareAccelerated: hardware,
                // VideoToolbox declares no dimension ceiling, and inventing
                // one would reject sources that in fact play. The core reads
                // `nil` as "no declared ceiling", which is the truth.
                maxWidth: nil,
                maxHeight: nil,
                maxBitrateBps: nil,
                supportsHdr10: hardware && candidate.fourCC != "avc1",
                // Dolby Vision profiles that need a display, not just a
                // decoder; asking the decoder alone would overstate it.
                supportsDolbyVision: hardware && candidate.fourCC == "hvc1",
                supports10Bit: hardware && candidate.fourCC != "avc1"
            )
        }
    }

    /// Codecs Apple can decode in software when there is no hardware path.
    private static let softwareDecodable: Set<String> = ["avc1", "hvc1", "mp4v", "mp2v"]

    private static func audioDecoders() -> [DecoderCapability] {
        audioCandidates.compactMap { candidate in
            guard hasDecoder(for: candidate.id) else { return nil }
            return DecoderCapability(
                mimeType: candidate.fourCC,
                // Audio decoding is not meaningfully a hardware decision on
                // Apple platforms, and reporting `false` would have the core
                // treat every audio track as a last resort.
                isHardwareAccelerated: true,
                maxWidth: nil,
                maxHeight: nil,
                maxBitrateBps: nil,
                supportsHdr10: false,
                supportsDolbyVision: false,
                supports10Bit: false
            )
        }
    }

    /// Ask Core Audio whether a decoder for `format` is actually installed,
    /// rather than assuming from the constant's existence.
    private static func hasDecoder(for format: AudioFormatID) -> Bool {
        var identifier = format
        var size: UInt32 = 0
        let status = AudioFormatGetPropertyInfo(
            kAudioFormatProperty_DecodeFormatIDs,
            UInt32(MemoryLayout<AudioFormatID>.size),
            &identifier,
            &size
        )
        guard status == noErr, size > 0 else {
            // Linear PCM has no decoder to install; it is always available.
            return format == kAudioFormatLinearPCM
        }

        var supported = [AudioFormatID](
            repeating: 0, count: Int(size) / MemoryLayout<AudioFormatID>.size)
        let readStatus = AudioFormatGetProperty(
            kAudioFormatProperty_DecodeFormatIDs,
            UInt32(MemoryLayout<AudioFormatID>.size),
            &identifier,
            &size,
            &supported
        )
        guard readStatus == noErr else { return format == kAudioFormatLinearPCM }
        return supported.contains(format)
    }

    private static func preferredAudioLanguages() -> [String] {
        Locale.preferredLanguages.compactMap {
            Locale(identifier: $0).language.languageCode?.identifier
        }
    }
}

/// What the screen can show, separated from how it is discovered so a test can
/// supply one without a running window server.
public struct DisplayCapabilities: Equatable, Sendable {
    /// Width in pixels.
    public let width: UInt32
    /// Height in pixels.
    public let height: UInt32
    /// Whether the display can present high dynamic range.
    public let supportsHDR: Bool

    /// Memberwise.
    public init(width: UInt32, height: UInt32, supportsHDR: Bool) {
        self.width = width
        self.height = height
        self.supportsHDR = supportsHDR
    }

    /// The device's own screen.
    @MainActor
    public static func current() -> DisplayCapabilities {
        #if canImport(UIKit)
        let screen = UIScreen.main
        let scale = screen.nativeScale
        let size = screen.nativeBounds.size
        return DisplayCapabilities(
            width: UInt32(max(size.width, size.height)),
            height: UInt32(min(size.width, size.height)),
            // `potentialEDRHeadroom` above 1 means the display can present
            // brighter-than-white, which is the property that matters --
            // rather than a model-name lookup that ages badly.
            supportsHDR: screen.potentialEDRHeadroom > 1.0
                || scale > 0 && screen.traitCollection.displayGamut == .P3
        )
        #elseif canImport(AppKit)
        guard let screen = NSScreen.main else {
            return DisplayCapabilities(width: 1920, height: 1080, supportsHDR: false)
        }
        let frame = screen.convertRectToBacking(screen.frame)
        return DisplayCapabilities(
            width: UInt32(frame.width),
            height: UInt32(frame.height),
            supportsHDR: screen.maximumPotentialExtendedDynamicRangeColorComponentValue > 1.0
        )
        #else
        return DisplayCapabilities(width: 1920, height: 1080, supportsHDR: false)
        #endif
    }
}
