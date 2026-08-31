import Foundation

/// The formatters every screen shares.
///
/// One place, because a duration rendered as "1:05:00" on one screen and
/// "1h 5m" on another is the kind of inconsistency nobody files a bug about
/// and everybody notices. Mirrors `core/ui/Format.kt`.
public enum BeamFormat {
    /// A duration in seconds, as a person reads it: "1h 42m", "8m", "45s".
    public static func duration(seconds: Double?) -> String {
        guard let seconds, seconds.isFinite, seconds > 0 else { return "--" }
        let total = Int(seconds.rounded())
        let hours = total / 3600
        let minutes = (total % 3600) / 60
        if hours > 0 {
            return minutes > 0 ? "\(hours)h \(minutes)m" : "\(hours)h"
        }
        if minutes > 0 {
            return "\(minutes)m"
        }
        return "\(total)s"
    }

    /// A playback position, as a player's time label: "1:05:00", "8:31".
    ///
    /// Distinct from ``duration(seconds:)`` on purpose: a transport control
    /// needs a monospaced clock that does not change width as it counts, and
    /// "1h 5m" cannot express seconds.
    public static func timecode(seconds: Double) -> String {
        guard seconds.isFinite, seconds >= 0 else { return "0:00" }
        let total = Int(seconds)
        let hours = total / 3600
        let minutes = (total % 3600) / 60
        let secs = total % 60
        if hours > 0 {
            return String(format: "%d:%02d:%02d", hours, minutes, secs)
        }
        return String(format: "%d:%02d", minutes, secs)
    }

    /// A file size, in the units the platform itself would use.
    public static func fileSize(bytes: UInt64) -> String {
        ByteCountFormatStyle(style: .file).format(Int64(clamping: bytes))
    }

    /// A bit rate, as "8.5 Mbps" or "640 kbps".
    public static func bitrate(bitsPerSecond: UInt64?) -> String? {
        guard let bitsPerSecond, bitsPerSecond > 0 else { return nil }
        if bitsPerSecond >= 1_000_000 {
            return String(format: "%.1f Mbps", Double(bitsPerSecond) / 1_000_000)
        }
        return "\(bitsPerSecond / 1000) kbps"
    }

    /// A resolution, named the way a viewer would name it.
    ///
    /// Keyed on **width**, not height. A 2.39:1 scope rip of a 1080p master is
    /// 1920x804: its height sits in the 720p band, but nobody calls that file
    /// 720p, and a source picker that did would have a viewer choose the
    /// "higher quality" 1280x720 version of the same film. Width is stable
    /// across aspect ratios in a way height is not.
    ///
    /// Lower bounds rather than exact matches, because no two rips of the same
    /// master agree on the last few pixels.
    public static func resolution(width: UInt32?, height: UInt32?) -> String? {
        if let width, width > 0 {
            switch width {
            case 3400...: return "4K"
            case 2400..<3400: return "1440p"
            case 1800..<2400: return "1080p"
            case 1200..<1800: return "720p"
            case 640..<1200: return "480p"
            default: break
            }
        }
        // Below the smallest named band, or with no width at all, the exact
        // dimensions say more than a name that would be wrong.
        switch (width, height) {
        case (let width?, let height?) where width > 0 && height > 0:
            return "\(width)x\(height)"
        case (_, let height?) where height > 0:
            return "\(height)p"
        default:
            return nil
        }
    }

    /// A language code as its own name, so "ja" reads as "Japanese".
    public static func language(code: String?) -> String? {
        guard let code, !code.isEmpty else { return nil }
        return Locale.current.localizedString(forLanguageCode: code) ?? code.uppercased()
    }
}
