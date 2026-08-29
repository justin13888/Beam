package dev.beam.android.core.ffi.preferences

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode
import dev.beam.android.core.model.UserPreferences
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import javax.inject.Inject
import javax.inject.Singleton

/** The user's settings, as an observable value. */
public interface PreferencesRepository {
    /** The current preferences, re-emitted on every change. */
    public val preferences: Flow<UserPreferences>

    /** Apply a change. The lambda receives the current value. */
    public suspend fun update(transform: (UserPreferences) -> UserPreferences)
}

@Singleton
internal class DataStorePreferencesRepository @Inject constructor(
    private val dataStore: DataStore<Preferences>,
) : PreferencesRepository {

    override val preferences: Flow<UserPreferences> = dataStore.data.map { stored ->
        UserPreferences(
            // An unrecognised stored value falls back to the default rather
            // than throwing: a downgrade after a new option ships must not
            // make the app unopenable.
            themeMode = stored[ThemeModeKey].toEnum(ThemeMode.System),
            paletteSource = stored[PaletteKey].toEnum(PaletteSource.Dynamic),
            quality = stored[QualityKey].toEnum(QualityPreference.Best),
            autoPlayNext = stored[AutoPlayKey] ?: true,
            allowSoftwareDecode = stored[SoftwareDecodeKey] ?: false,
            preferredAudioLanguages = stored[AudioLanguagesKey]
                ?.split(',')
                ?.filter { it.isNotBlank() }
                ?: emptyList(),
            downloadOverCellular = stored[CellularDownloadsKey] ?: false,
        )
    }

    override suspend fun update(transform: (UserPreferences) -> UserPreferences) {
        dataStore.edit { stored ->
            val current = UserPreferences(
                themeMode = stored[ThemeModeKey].toEnum(ThemeMode.System),
                paletteSource = stored[PaletteKey].toEnum(PaletteSource.Dynamic),
                quality = stored[QualityKey].toEnum(QualityPreference.Best),
                autoPlayNext = stored[AutoPlayKey] ?: true,
                allowSoftwareDecode = stored[SoftwareDecodeKey] ?: false,
                preferredAudioLanguages = stored[AudioLanguagesKey]
                    ?.split(',')
                    ?.filter { it.isNotBlank() }
                    ?: emptyList(),
                downloadOverCellular = stored[CellularDownloadsKey] ?: false,
            )
            val next = transform(current)
            stored[ThemeModeKey] = next.themeMode.name
            stored[PaletteKey] = next.paletteSource.name
            stored[QualityKey] = next.quality.name
            stored[AutoPlayKey] = next.autoPlayNext
            stored[SoftwareDecodeKey] = next.allowSoftwareDecode
            stored[AudioLanguagesKey] = next.preferredAudioLanguages.joinToString(",")
            stored[CellularDownloadsKey] = next.downloadOverCellular
        }
    }

    private companion object {
        val ThemeModeKey = stringPreferencesKey("pref:theme_mode")
        val PaletteKey = stringPreferencesKey("pref:palette")
        val QualityKey = stringPreferencesKey("pref:quality")
        val AutoPlayKey = booleanPreferencesKey("pref:auto_play_next")
        val SoftwareDecodeKey = booleanPreferencesKey("pref:allow_software_decode")
        val AudioLanguagesKey = stringPreferencesKey("pref:audio_languages")
        val CellularDownloadsKey = booleanPreferencesKey("pref:cellular_downloads")

        inline fun <reified T : Enum<T>> String?.toEnum(fallback: T): T =
            this?.let { name -> enumValues<T>().firstOrNull { it.name == name } } ?: fallback
    }
}
