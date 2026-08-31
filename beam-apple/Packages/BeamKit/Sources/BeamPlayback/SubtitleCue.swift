import BeamFFI
import Foundation

/// One line of text subtitle, with the window it should be on screen for.
///
/// Only text subtitles are rendered. PGS and VobSub are bitmap formats -- they
/// carry compressed images with their own palettes and timing, and decoding
/// them is a second image pipeline rather than a parsing detail. They are
/// reported as present and unplayable rather than silently dropped, which is
/// the same treatment `capability::select` gives an undecodable video source.
public struct SubtitleCue: Equatable, Sendable {
    /// When it appears.
    public let start: Double
    /// When it goes.
    public let end: Double
    /// The text, with markup stripped.
    public let text: String

    /// Memberwise.
    public init(start: Double, end: Double, text: String) {
        self.start = start
        self.end = end
        self.text = text
    }

    /// Whether this cue covers `seconds`.
    public func contains(_ seconds: Double) -> Bool {
        seconds >= start && seconds < end
    }

    /// Whether this engine can render the track at all.
    public static func isRenderable(_ track: ExtractorTrack) -> Bool {
        switch track.subtitleFormat {
        case .subRip, .ass, .webVtt: true
        case .pgs, .vobSub, .unknown, .none: false
        }
    }

    /// Build a cue from a demuxed subtitle sample.
    ///
    /// Matroska stores each subtitle line as its own timed sample, so the
    /// container has already done the parsing an `.srt` file would need -- what
    /// is left is stripping the markup the format wraps around the words.
    public static func from(sample: EncodedSample, format: SubtitleFormat?) -> SubtitleCue? {
        guard let raw = String(data: sample.data, encoding: .utf8) else { return nil }
        let text: String
        switch format {
        case .ass:
            text = strippingASSMarkup(raw)
        case .subRip, .webVtt:
            text = strippingTags(raw)
        default:
            return nil
        }

        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }

        // A subtitle sample without a duration would otherwise be shown for a
        // single frame. Three seconds is the usual reading-time default and is
        // what players fall back to.
        let duration = sample.durationSeconds ?? 3.0
        return SubtitleCue(
            start: sample.ptsSeconds,
            end: sample.ptsSeconds + duration,
            text: trimmed
        )
    }

    /// Strip an ASS dialogue line down to its words.
    ///
    /// Matroska stores ASS as the comma-separated fields after `Dialogue:`,
    /// with the text last -- and the text itself may contain commas, so the
    /// split is bounded to the nine leading fields rather than greedy.
    static func strippingASSMarkup(_ raw: String) -> String {
        let fields = raw.split(separator: ",", maxSplits: 8, omittingEmptySubsequences: false)
        let body = fields.count > 8 ? String(fields[8]) : raw
        return
            body
            .replacingOccurrences(of: "\\N", with: "\n")
            .replacingOccurrences(of: "\\n", with: "\n")
            // Override blocks such as {\an8} or {\i1}: positioning and styling
            // this renderer does not implement, and showing the braces would
            // be worse than dropping them.
            .replacingOccurrences(of: "\\{[^}]*\\}", with: "", options: .regularExpression)
    }

    /// Strip SubRip and WebVTT inline tags.
    static func strippingTags(_ raw: String) -> String {
        raw.replacingOccurrences(of: "<[^>]+>", with: "", options: .regularExpression)
    }
}
