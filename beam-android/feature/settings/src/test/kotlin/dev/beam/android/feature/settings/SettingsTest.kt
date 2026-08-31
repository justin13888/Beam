package dev.beam.android.feature.settings

import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.FakeSessionRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class SettingsTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun viewModel(
        preferences: FakePreferencesRepository = FakePreferencesRepository(),
        servers: FakeServerRepository = FakeServerRepository(),
        sessions: FakeSessionRepository = FakeSessionRepository(),
    ) = SettingsViewModel(preferences, servers, sessions)

    @Test
    fun `preferences are shown and changes are written through`() =
        runTest {
            val preferences = FakePreferencesRepository()
            val model = viewModel(preferences = preferences)
            testScheduler.advanceUntilIdle()

            model.setThemeMode(ThemeMode.Dark)
            model.setQuality(QualityPreference.Smallest)
            model.setAutoPlayNext(false)
            testScheduler.advanceUntilIdle()

            assertEquals(ThemeMode.Dark, preferences.current.themeMode)
            assertEquals(QualityPreference.Smallest, preferences.current.quality)
            assertFalse(preferences.current.autoPlayNext)
        }

    @Test
    fun `a preference change is one write, not a read-modify-write per field`() =
        runTest {
            val preferences = FakePreferencesRepository()
            val model = viewModel(preferences = preferences)
            testScheduler.advanceUntilIdle()

            model.setPaletteSource(PaletteSource.Brand)
            testScheduler.advanceUntilIdle()

            assertEquals(1, preferences.updateCount)
        }

    @Test
    fun `the account and its devices are loaded`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            val state = model.state.value
            assertNotNull(state.server)
            assertTrue(state.sessions.isNotEmpty())
        }

    @Test
    fun `trusted certificates are listed for the active server`() =
        runTest {
            val servers = FakeServerRepository()
            val active = servers.restore().first { it.isActive }
            servers.trustCertificate(active.id, "AA:BB:CC")

            val model = viewModel(servers = servers)
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("AA:BB:CC"), model.state.value.trustedCertificates)
        }

    @Test
    fun `forgetting certificates withdraws every one of them`() =
        runTest {
            // A trust decision made once, possibly hastily, has to be reversible
            // without reinstalling the app.
            val servers = FakeServerRepository()
            val active = servers.restore().first { it.isActive }
            servers.trustCertificate(active.id, "AA:BB:CC")

            val model = viewModel(servers = servers)
            testScheduler.advanceUntilIdle()
            model.forgetCertificates()
            testScheduler.advanceUntilIdle()

            assertTrue(
                model.state.value.trustedCertificates
                    .isEmpty(),
            )
            assertTrue(servers.trustedCertificates(active.id).isEmpty())
        }

    @Test
    fun `signing out marks the session ended so the shell can navigate`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            model.signOut()
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value.isSignedOut)
        }

    @Test
    fun `signing out everywhere also ends this session`() =
        runTest {
            // The dialog promises "including this one"; a viewer left apparently
            // signed in on the device they pressed it from would be a lie.
            val sessions = FakeSessionRepository()
            val model = viewModel(sessions = sessions)
            testScheduler.advanceUntilIdle()

            model.signOutEverywhere()
            testScheduler.advanceUntilIdle()

            assertTrue(model.state.value.isSignedOut)
        }

    @Test
    fun `revoking a device removes it from the list`() =
        runTest {
            val sessions = FakeSessionRepository()
            val model = viewModel(sessions = sessions)
            testScheduler.advanceUntilIdle()
            val victim =
                model.state.value.sessions
                    .first()

            model.revokeSession(victim.id)
            testScheduler.advanceUntilIdle()

            assertFalse(
                model.state.value.sessions
                    .any { it.id == victim.id },
            )
        }

    @Test
    fun `every preference label reads as words rather than an enum name`() {
        ThemeMode.entries.forEach { assertTrue(it.label().first().isUpperCase()) }
        PaletteSource.entries.forEach { assertTrue(it.label().contains(' ')) }
        QualityPreference.entries.forEach { assertTrue(it.label().contains(' ')) }
    }
}
