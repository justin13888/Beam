import BeamUI
import Foundation
import Testing

/// The formatters every screen shares.
@Suite("Formatting")
struct FormattingTests {
    @Test(
        "a duration reads the way a person says it",
        arguments: [
            (6240.0, "1h 44m"),
            (3600.0, "1h"),
            (510.0, "8m"),
            (45.0, "45s"),
        ]
    )
    func durations(seconds: Double, expected: String) {
        #expect(BeamFormat.duration(seconds: seconds) == expected)
    }

    @Test("an absent or nonsensical duration is not rendered as zero")
    func missingDuration() {
        // "0s" would read as a file of no length, which is a different claim
        // from "the server did not say".
        #expect(BeamFormat.duration(seconds: nil) == "--")
        #expect(BeamFormat.duration(seconds: 0) == "--")
        #expect(BeamFormat.duration(seconds: .nan) == "--")
        #expect(BeamFormat.duration(seconds: -5) == "--")
    }

    @Test(
        "a timecode is a clock that does not change width as it counts",
        arguments: [
            (0.0, "0:00"),
            (61.0, "1:01"),
            (511.0, "8:31"),
            (3900.0, "1:05:00"),
        ]
    )
    func timecodes(seconds: Double, expected: String) {
        #expect(BeamFormat.timecode(seconds: seconds) == expected)
    }

    @Test(
        "a resolution is named the way a viewer names it",
        arguments: [
            (3840 as UInt32, 2160 as UInt32, "4K"),
            (1920, 1080, "1080p"),
            // A 2.39:1 scope rip of a 1080p master is 1920x804. Its *height*
            // sits in the 720p band, so keying on height would have a viewer
            // choose the "higher quality" 1280x720 version of the same film.
            (1920, 804, "1080p"),
            (1998, 1080, "1080p"),
            (1280, 720, "720p"),
            (854, 480, "480p"),
        ]
    )
    func resolutions(width: UInt32, height: UInt32, expected: String) {
        #expect(BeamFormat.resolution(width: width, height: height) == expected)
    }

    @Test("an unusual resolution falls back to its dimensions rather than a wrong name")
    func unusualResolution() {
        #expect(BeamFormat.resolution(width: 320, height: 240) == "320x240")
        #expect(BeamFormat.resolution(width: nil, height: nil) == nil)
    }

    @Test("a bit rate switches units where a person would")
    func bitrates() {
        #expect(BeamFormat.bitrate(bitsPerSecond: 8_500_000) == "8.5 Mbps")
        #expect(BeamFormat.bitrate(bitsPerSecond: 640_000) == "640 kbps")
        #expect(BeamFormat.bitrate(bitsPerSecond: nil) == nil)
        #expect(BeamFormat.bitrate(bitsPerSecond: 0) == nil)
    }
}
