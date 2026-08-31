import Foundation

/// How the app should pick a colour scheme.
public enum ThemeMode: String, CaseIterable, Codable, Sendable {
    /// Follow the system setting.
    case system
    /// Always light.
    case light
    /// Always dark.
    case dark
}

/// Which source a title should play at, when several exist.
///
/// Maps onto the core's `QualityPolicy`; kept as a separate type so the stored
/// preference does not carry a generated enum into `UserDefaults`, where a
/// future rename of the FFI type would silently invalidate it.
public enum QualityPreference: String, CaseIterable, Codable, Sendable {
    /// The largest source the device can decode.
    case best
    /// The source closest to the screen's own resolution.
    case matchScreen
    /// The smallest playable source.
    case smallest
}

/// Everything the app remembers about how this person wants it to behave.
public struct UserPreferences: Equatable, Codable, Sendable {
    /// The colour scheme.
    public var theme: ThemeMode
    /// Which source to prefer.
    public var quality: QualityPreference
    /// Whether to play the next episode automatically.
    public var autoplayNextEpisode: Bool
    /// Whether sources that only a software decoder can handle are offered.
    ///
    /// Off by default. A software decode of 4K HEVC on a phone is a warm
    /// device and a flat battery, so it is a choice rather than a fallback.
    public var allowSoftwareDecode: Bool
    /// Whether downloads may run without Wi-Fi.
    public var allowCellularDownloads: Bool

    /// The defaults a fresh install starts from.
    public static let `default` = UserPreferences(
        theme: .system,
        quality: .best,
        autoplayNextEpisode: true,
        allowSoftwareDecode: false,
        allowCellularDownloads: false
    )

    /// Memberwise, so a caller can build one without every field.
    public init(
        theme: ThemeMode,
        quality: QualityPreference,
        autoplayNextEpisode: Bool,
        allowSoftwareDecode: Bool,
        allowCellularDownloads: Bool
    ) {
        self.theme = theme
        self.quality = quality
        self.autoplayNextEpisode = autoplayNextEpisode
        self.allowSoftwareDecode = allowSoftwareDecode
        self.allowCellularDownloads = allowCellularDownloads
    }
}
