package dev.beam.android.core.model

/** Which colour scheme to render in. */
public enum class ThemeMode {
    /** Follow the system setting. */
    System,

    /** Always light. */
    Light,

    /** Always dark. */
    Dark,
}

/**
 * Where the palette comes from.
 *
 * Dynamic colour is the platform default on Android 12 and later and is what
 * makes an app feel native, so it is the default here. Some users want their
 * media app to look like itself regardless of the wallpaper, hence the choice.
 */
public enum class PaletteSource {
    /** Derive the palette from the wallpaper, where the platform supports it. */
    Dynamic,

    /** Use Beam's own palette. */
    Brand,
}

/** Which source the player reaches for first. */
public enum class QualityPreference {
    /** The highest-quality file this device can actually decode. */
    Best,

    /** The file closest to the screen's own resolution. */
    MatchScreen,

    /** The smallest playable file, for a metered connection. */
    Smallest,
}

/**
 * Everything the user can change about how the app looks and plays.
 *
 * One immutable record so a settings screen renders from a single value and a
 * change is one atomic write rather than a sequence a reader can observe
 * halfway through.
 */
public data class UserPreferences(
    /** Which colour scheme to render in. */
    val themeMode: ThemeMode = ThemeMode.System,
    /** Where the palette comes from. */
    val paletteSource: PaletteSource = PaletteSource.Dynamic,
    /** Which source the player reaches for first. */
    val quality: QualityPreference = QualityPreference.Best,
    /** Whether the next episode starts on its own. */
    val autoPlayNext: Boolean = true,
    /**
     * Whether a file this device can only decode in software may still play.
     *
     * Off by default: software decoding a 4K HEVC stream drains the battery
     * and usually stutters, and Beam never transcodes, so the honest answer is
     * to mark the file unplayable and say why.
     */
    val allowSoftwareDecode: Boolean = false,
    /** Preferred audio languages, best first, as ISO 639 codes. */
    val preferredAudioLanguages: List<String> = emptyList(),
    /** Whether downloads may run without Wi-Fi. */
    val downloadOverCellular: Boolean = false,
)
