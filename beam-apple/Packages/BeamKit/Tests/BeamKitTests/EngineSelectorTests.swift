import BeamFFI
import BeamTesting
import Foundation
import Testing

@testable import BeamPlayback

/// Which engine plays what.
///
/// The consequential test in this file is the last one: the set of containers
/// routed to the sample-buffer engine is derived from the core's own
/// `probeContainers()` rather than restated here, so the two cannot drift.
/// A hand-maintained copy would pass while the demuxer lost a container, and
/// the symptom would be a file routed to an engine that refuses it.
@Suite("Engine selection")
struct EngineSelectorTests {
    @Test(
        "containers AVFoundation can open go to AVPlayer",
        arguments: ["mp4", "m4v", "mov", "MP4", ".mov"]
    )
    func nativeContainersUseAVPlayer(container: String) {
        #expect(EngineSelector.engine(forContainer: container) == .avPlayer)
    }

    @Test(
        "containers only our demuxer can open go to the sample-buffer engine",
        arguments: ["mkv", "webm", "MKV", ".mkv"]
    )
    func demuxedContainersUseSampleBuffer(container: String) {
        #expect(EngineSelector.engine(forContainer: container) == .sampleBuffer)
    }

    @Test("an unknown container falls back to AVPlayer")
    func unknownContainerFallsBack() {
        // AVFoundation sniffs content and can often open a file whose
        // container Beam failed to record. The Matroska extractor would simply
        // refuse anything that is not Matroska, so guessing wrong in that
        // direction costs a file that would have played.
        #expect(EngineSelector.engine(forContainer: nil) == .avPlayer)
        #expect(EngineSelector.engine(forContainer: "") == .avPlayer)
        #expect(EngineSelector.engine(forContainer: "wmv") == .avPlayer)
    }

    @Test("the URL extension decides when the catalogue does not know")
    func fileExtensionIsTheFallback() {
        #expect(
            EngineSelector.engine(forContainer: nil, fileExtension: "mkv") == .sampleBuffer
        )
        // The catalogue's answer wins over the URL's: the server probed the
        // file, and a `/stream` URL's extension is often absent or wrong.
        #expect(
            EngineSelector.engine(forContainer: "mp4", fileExtension: "mkv") == .avPlayer
        )
    }

    @Test("a source's container routes it")
    func sourceRouting() {
        let mkv = Fixtures.source(container: "mkv")
        let mp4 = Fixtures.source(container: "mp4")

        #expect(EngineSelector.engine(for: mkv) == .sampleBuffer)
        #expect(EngineSelector.engine(for: mp4) == .avPlayer)
    }

    @Test("every container the core can demux is routed to the demuxing engine")
    func everyDemuxableContainerIsRouted() {
        // Derived from the core rather than restated: teaching the extractor a
        // new container should route it here with no change to this file, and
        // removing one should stop it being claimed.
        for container in probeContainers() {
            #expect(
                EngineSelector.engine(forContainer: container) == .sampleBuffer,
                "\(container) is demuxable but was routed to AVPlayer"
            )
        }
    }
}
