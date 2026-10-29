package dev.beam.android.feature.settings

import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode
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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.SessionState

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
    fun `a failed sign-out everywhere is reported, not claimed`() =
        runTest {
            // The other devices are still signed in. Leaving the screen as
            // though they were not would tell the viewer a stolen phone is
            // locked out when it is not.
            val sessions =
                FakeSessionRepository().apply {
                    failWith = BeamException.Network("the connection dropped", true)
                }
            val model = viewModel(sessions = sessions)
            testScheduler.advanceUntilIdle()

            model.signOutEverywhere()
            testScheduler.advanceUntilIdle()

            assertFalse("nothing was signed out", sessions.loggedOutEverywhere)
            assertFalse("so the viewer is not sent away as though it were", model.state.value.isSignedOut)
            val error = model.state.value.error
            assertNotNull("and is told", error)
            assertTrue(error!!.startsWith("Could not sign out everywhere."))

            model.clearError()
            assertNull(model.state.value.error)
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

    private fun knownServers() =
        FakeServerRepository(
            listOf(
                Fixtures.server(id = "home", displayName = "Home"),
                Fixtures.server(id = "cabin", displayName = "Cabin", isActive = false),
                Fixtures.server(
                    id = "office",
                    displayName = "Office",
                    isActive = false,
                    state = SessionState.LoggedOut,
                ),
            ),
        )

    @Test
    fun `every known server is listed, the active one marked`() =
        runTest {
            val model = viewModel(servers = knownServers())
            testScheduler.advanceUntilIdle()

            val listed = model.state.value.servers
            assertEquals(listOf("home", "cabin", "office"), listed.map { it.id })
            assertEquals("home", listed.single { it.isActive }.id)
        }

    @Test
    fun `switching to a server signed in to starts the app afresh against it`() =
        runTest {
            val servers = knownServers()
            val model = viewModel(servers = servers)
            testScheduler.advanceUntilIdle()

            model.switchServer("cabin")
            testScheduler.advanceUntilIdle()

            assertEquals("cabin", servers.activeServer()?.id)
            assertTrue(model.state.value.hasSwitchedServer)
            assertFalse(model.state.value.isSignedOut)
        }

    @Test
    fun `switching to a server not signed in to goes to sign-in first`() =
        runTest {
            val servers = knownServers()
            val model = viewModel(servers = servers)
            testScheduler.advanceUntilIdle()

            model.switchServer("office")
            testScheduler.advanceUntilIdle()

            assertEquals("office", servers.activeServer()?.id)
            assertTrue(model.state.value.isSignedOut)
            assertFalse(model.state.value.hasSwitchedServer)
        }

    @Test
    fun `choosing the server already in use changes nothing`() =
        runTest {
            val model = viewModel(servers = knownServers())
            testScheduler.advanceUntilIdle()

            model.switchServer("home")
            testScheduler.advanceUntilIdle()

            assertFalse(model.state.value.hasSwitchedServer)
            assertFalse(model.state.value.isSignedOut)
        }

    @Test
    fun `an unused server can be forgotten, the one in use cannot`() =
        runTest {
            val servers = knownServers()
            val model = viewModel(servers = servers)
            testScheduler.advanceUntilIdle()

            model.removeServer("cabin")
            model.removeServer("home")
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("home", "office"),
                model.state.value.servers
                    .map { it.id },
            )
        }

    @Test
    fun `every preference label reads as words rather than an enum name`() {
        ThemeMode.entries.forEach { assertTrue(it.label().first().isUpperCase()) }
        PaletteSource.entries.forEach { assertTrue(it.label().contains(' ')) }
        QualityPreference.entries.forEach { assertTrue(it.label().contains(' ')) }
    }
}
