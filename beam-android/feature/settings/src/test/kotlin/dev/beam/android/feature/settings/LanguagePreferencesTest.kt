package dev.beam.android.feature.settings

import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.FakeSessionRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.LanguagePreferences
import uniffi.beam_client_core.SessionState
import uniffi.beam_client_core.SubtitleMode

/** The language section of settings (issue #329), over the account's fakes. */
@OptIn(ExperimentalCoroutinesApi::class)
class LanguagePreferencesTest {
    private val dispatcher = StandardTestDispatcher()
    private val sessions = FakeSessionRepository()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun viewModel(
        servers: FakeServerRepository = FakeServerRepository(listOf(Fixtures.server()), restored = true),
    ) = SettingsViewModel(FakePreferencesRepository(), servers, sessions)

    @Test
    fun `the stored preferences are read with the account`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            assertEquals(sessions.languages, model.state.value.languages)
        }

    @Test
    fun `a signed-out viewer has no preferences to show`() =
        runTest {
            val model =
                viewModel(
                    FakeServerRepository(
                        listOf(Fixtures.server(state = SessionState.LoggedOut)),
                        restored = true,
                    ),
                )
            testScheduler.advanceUntilIdle()

            assertNull(model.state.value.languages)
        }

    @Test
    fun `a change is stored on the server and shown as the server stored it`() =
        runTest {
            sessions.stored =
                { sent -> sent.copy(audioLanguages = sent.audioLanguages.map { if (it == "ja") "jpn" else it }) }
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            model.setLanguagePreferences(
                LanguagePreferences(listOf("ja", "eng"), listOf("eng"), SubtitleMode.ON),
            )
            testScheduler.advanceUntilIdle()

            val expected = LanguagePreferences(listOf("jpn", "eng"), listOf("eng"), SubtitleMode.ON)
            assertEquals(expected, sessions.languages)
            assertEquals(expected, model.state.value.languages)
        }

    @Test
    fun `a refused change is said and leaves the shown preferences as they were`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()
            val before = model.state.value.languages
            sessions.failLanguageWritesWith =
                BeamException.Server(422u, false, "\"klingon\" is not a language", "about:blank")

            model.setLanguagePreferences(LanguagePreferences(listOf("klingon"), emptyList(), SubtitleMode.OFF))
            testScheduler.advanceUntilIdle()

            assertEquals(before, model.state.value.languages)
            assertNotNull(model.state.value.error)
        }

    @Test
    fun `typed languages are split at commas, trimmed, in order, without blanks`() {
        assertEquals(listOf("ja", "English", "en-GB"), parseLanguages(" ja, English ,, en-GB ,"))
        assertEquals(emptyList<String>(), parseLanguages("  "))
    }
}
