import BeamFFI
import Foundation
import Testing

@testable import BeamPlayback

/// Turning demuxed subtitle samples into text on screen.
@Suite("Subtitle cues")
struct SubtitleCueTests {
    private func sample(_ text: String, duration: Double? = 2) -> EncodedSample {
        EncodedSample(
            track: 3,
            data: Data(text.utf8),
            ptsSeconds: 10,
            durationSeconds: duration,
            isKeyframe: true
        )
    }

    @Test("a SubRip cue keeps its words and drops its tags")
    func subRip() {
        let cue = SubtitleCue.from(
            sample: sample("<i>I never knew the old Vienna</i>"),
            format: .subRip
        )

        #expect(cue?.text == "I never knew the old Vienna")
        #expect(cue?.start == 10)
        #expect(cue?.end == 12)
    }

    @Test("an ASS dialogue line is reduced to its text")
    func assDialogue() {
        // Matroska stores the fields after `Dialogue:`, with the text last --
        // and the text itself contains commas, which is why the split is
        // bounded rather than greedy.
        let raw = "0,0,Default,,0,0,0,,{\\an8}Hello, there, friend"
        let cue = SubtitleCue.from(sample: sample(raw), format: .ass)

        #expect(cue?.text == "Hello, there, friend")
    }

    @Test("an ASS line break becomes a real one")
    func assLineBreak() {
        let raw = "0,0,Default,,0,0,0,,First line\\NSecond line"
        let cue = SubtitleCue.from(sample: sample(raw), format: .ass)

        #expect(cue?.text == "First line\nSecond line")
    }

    @Test("a cue with no duration is shown long enough to read")
    func defaultDuration() {
        // A subtitle sample without a duration would otherwise flash for a
        // single frame.
        let cue = SubtitleCue.from(sample: sample("Line", duration: nil), format: .subRip)

        #expect(cue?.end == 13)
    }

    @Test("an empty cue produces nothing rather than an empty box")
    func emptyCue() {
        #expect(SubtitleCue.from(sample: sample("   "), format: .subRip) == nil)
    }

    @Test("a cue covers its own window and nothing outside it")
    func containment() {
        let cue = SubtitleCue(start: 10, end: 12, text: "Line")

        #expect(!cue.contains(9.9))
        #expect(cue.contains(10))
        #expect(cue.contains(11.9))
        // Half-open: at exactly the end the next cue may already have started,
        // and showing both would stack two lines in one place.
        #expect(!cue.contains(12))
    }

    @Test("bitmap subtitle formats are reported unrenderable rather than dropped")
    func bitmapFormats() {
        // PGS and VobSub carry compressed images with their own palettes.
        // Decoding them is a second image pipeline, not a parsing detail, so
        // they are reported present and unsupported -- the same treatment an
        // undecodable video source gets.
        for format in [SubtitleFormat.pgs, .vobSub, .unknown] {
            let track = ExtractorTrack(
                number: 3,
                kind: .subtitle,
                codecId: "S_HDMV/PGS",
                videoCodec: nil,
                audioCodec: nil,
                subtitleFormat: format,
                codecPrivate: Data(),
                width: 0,
                height: 0,
                sampleRate: 0,
                channels: 0,
                language: "eng",
                name: nil,
                isDefault: false,
                isForced: false
            )
            #expect(!SubtitleCue.isRenderable(track))
        }
    }

    @Test("text subtitle formats are renderable")
    func textFormats() {
        for format in [SubtitleFormat.subRip, .ass, .webVtt] {
            let track = ExtractorTrack(
                number: 3,
                kind: .subtitle,
                codecId: "S_TEXT/UTF8",
                videoCodec: nil,
                audioCodec: nil,
                subtitleFormat: format,
                codecPrivate: Data(),
                width: 0,
                height: 0,
                sampleRate: 0,
                channels: 0,
                language: "eng",
                name: nil,
                isDefault: false,
                isForced: false
            )
            #expect(SubtitleCue.isRenderable(track))
        }
    }
}
