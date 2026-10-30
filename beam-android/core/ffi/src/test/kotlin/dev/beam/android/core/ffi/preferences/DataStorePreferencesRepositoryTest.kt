package dev.beam.android.core.ffi.preferences

import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import dev.beam.android.core.ffi.storage.DataStoreFiles
import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode
import dev.beam.android.core.model.UserPreferences
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test

/** The viewer's settings, through a real DataStore file. */
class DataStorePreferencesRepositoryTest {
    @get:Rule
    val files = DataStoreFiles()

    @Test
    fun `nothing stored reads as the defaults`() =
        runTest {
            val repository = DataStorePreferencesRepository(files.open())

            assertEquals(UserPreferences(), repository.preferences.first())
        }

    @Test
    fun `preferences survive a new instance`() =
        runTest {
            // Every field moved off its default, so a field the write drops or
            // the read ignores comes back as the default and fails here.
            val chosen =
                UserPreferences(
                    themeMode = ThemeMode.Dark,
                    paletteSource = PaletteSource.Brand,
                    quality = QualityPreference.Smallest,
                    autoPlayNext = false,
                    allowSoftwareDecode = true,
                    preferredAudioLanguages = listOf("fra", "eng"),
                    downloadOverCellular = true,
                )
            DataStorePreferencesRepository(files.open()).update { chosen }

            val restarted = DataStorePreferencesRepository(files.reopen())

            assertEquals(chosen, restarted.preferences.first())
        }

    @Test
    fun `an update sees the stored value, not the defaults`() =
        runTest {
            val repository = DataStorePreferencesRepository(files.open())
            repository.update { it.copy(themeMode = ThemeMode.Light) }

            repository.update { it.copy(quality = QualityPreference.MatchScreen) }

            val stored = repository.preferences.first()
            assertEquals(ThemeMode.Light, stored.themeMode)
            assertEquals(QualityPreference.MatchScreen, stored.quality)
        }

    @Test
    fun `an unknown stored theme falls back to system`() =
        runTest {
            // What a downgrade leaves behind once a newer build has stored an
            // option this one does not know: the app must still open.
            val dataStore = files.open()
            dataStore.edit {
                it[stringPreferencesKey("pref:theme_mode")] = "Sepia"
                it[stringPreferencesKey("pref:palette")] = "Neon"
                it[stringPreferencesKey("pref:quality")] = "Ludicrous"
            }

            val read = DataStorePreferencesRepository(dataStore).preferences.first()

            assertEquals(ThemeMode.System, read.themeMode)
            assertEquals(PaletteSource.Dynamic, read.paletteSource)
            assertEquals(QualityPreference.Best, read.quality)
        }

    @Test
    fun `audio languages are stored comma-joined and read back without blanks`() =
        runTest {
            val dataStore = files.open()
            val repository = DataStorePreferencesRepository(dataStore)

            repository.update { it.copy(preferredAudioLanguages = listOf("jpn", "eng")) }
            assertEquals("jpn,eng", dataStore.data.first()[stringPreferencesKey("pref:audio_languages")])

            repository.update { it.copy(preferredAudioLanguages = emptyList()) }
            assertEquals(emptyList<String>(), repository.preferences.first().preferredAudioLanguages)

            dataStore.edit { it[stringPreferencesKey("pref:audio_languages")] = ",fra,,deu," }
            assertEquals(listOf("fra", "deu"), repository.preferences.first().preferredAudioLanguages)
        }
}
