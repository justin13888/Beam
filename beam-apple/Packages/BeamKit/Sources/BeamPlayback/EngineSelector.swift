import BeamFFI
import Foundation

/// Which engine should play a given source.
///
/// One pure function, and the only place the choice is made. It matters that
/// this is not spread across the player: the two engines have very different
/// capabilities, and a source routed to the wrong one fails in a way that
/// looks like a corrupt file rather than like a wrong decision.
public enum PlaybackEngineKind: String, Equatable, Sendable, CaseIterable {
    /// `AVPlayer`, with the system's own transport, PiP and AirPlay.
    case avPlayer
    /// Our demuxer feeding VideoToolbox through sample buffers.
    case sampleBuffer
}

/// Chooses between the engines.
public enum EngineSelector {
    /// Containers only our own extractor can open.
    ///
    /// Derived from the core's `probeContainers()` rather than restated, so a
    /// container the demuxer is later taught is routed here automatically and
    /// a container it loses stops being claimed.
    public static var demuxedContainers: Set<String> {
        Set(probeContainers())
    }

    /// Pick an engine for a container.
    ///
    /// - Parameters:
    ///   - container: the container, in any case and with or without a leading
    ///     dot. `nil` when the catalogue does not know.
    ///   - fileExtension: the URL's extension, used only when `container` is
    ///     absent.
    ///
    /// Defaults to ``PlaybackEngineKind/avPlayer`` when neither is known,
    /// because AVFoundation sniffs content and can often open a file whose
    /// container Beam failed to record -- whereas the Matroska extractor
    /// would simply refuse anything that is not Matroska.
    public static func engine(
        forContainer container: String?,
        fileExtension: String? = nil
    ) -> PlaybackEngineKind {
        // The catalogue's container wins outright when it has one. It came
        // from probing the file at index time, where the URL's extension is a
        // guess -- `/v1/files/{id}/stream` carries no extension at all, and a
        // download URL's filename is whatever was on disk. Letting the
        // extension override a known container would route a file to an engine
        // that then refuses it, which reads as corrupt media rather than as a
        // wrong decision.
        let candidate = normalise(container) ?? normalise(fileExtension)
        guard let candidate else { return .avPlayer }
        return demuxedContainers.contains(candidate) ? .sampleBuffer : .avPlayer
    }

    private static func normalise(_ value: String?) -> String? {
        guard let value else { return nil }
        var trimmed = value.trimmingCharacters(in: .whitespaces).lowercased()
        if trimmed.hasPrefix(".") { trimmed.removeFirst() }
        return trimmed.isEmpty ? nil : trimmed
    }

    /// Pick an engine for a source the catalogue described.
    public static func engine(for source: MediaSourceView) -> PlaybackEngineKind {
        engine(
            forContainer: source.container,
            fileExtension: URL(string: source.streamUrl)?.pathExtension
        )
    }

    /// Pick an engine for an item about to be played.
    public static func engine(for item: PlaybackItem) -> PlaybackEngineKind {
        engine(forContainer: item.container, fileExtension: item.url.pathExtension)
    }
}
