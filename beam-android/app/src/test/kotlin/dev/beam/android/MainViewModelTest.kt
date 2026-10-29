package dev.beam.android

import android.app.Application
import androidx.test.core.app.ApplicationProvider
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
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
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.SessionState

/**
 * Admin access follows the active server: it is per server, so a switch in
 * settings or a sign-in to another server changes it without a restart. So
 * does whether the viewer is signed in at all -- at a cold start, and when the
 * server refuses the session mid-use.
 *
 * Robolectric only for the [android.content.Context] the device profile is
 * built from; a plain [Application] keeps the app's own, which loads the
 * native core, out of the test.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@Config(application = Application::class)
class MainViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun viewModel(
        servers: FakeServerRepository = FakeServerRepository(listOf(Fixtures.server(isActive = true))),
        playback: FakePlaybackRepository = FakePlaybackRepository(),
        preferences: FakePreferencesRepository = FakePreferencesRepository(),
    ) = MainViewModel(
        servers = servers,
        playback = playback,
        preferences = preferences,
        context = ApplicationProvider.getApplicationContext(),
    )

    @Test
    fun `toggling software decode updates the core's profile`() =
        runTest(dispatcher) {
            val playback = FakePlaybackRepository()
            val preferences = FakePreferencesRepository(UserPreferences(allowSoftwareDecode = false))
            viewModel(playback = playback, preferences = preferences)
            testScheduler.advanceUntilIdle()
            assertEquals(false, playback.deviceProfile?.allowSoftwareDecode)

            preferences.update { it.copy(allowSoftwareDecode = true) }
            testScheduler.advanceUntilIdle()
            assertEquals(
                "the next selection may use a software decoder",
                true,
                playback.deviceProfile?.allowSoftwareDecode,
            )

            preferences.update { it.copy(allowSoftwareDecode = false) }
            testScheduler.advanceUntilIdle()
            assertEquals(
                "and turning it off again forbids it again",
                false,
                playback.deviceProfile?.allowSoftwareDecode,
            )
        }

    @Test
    fun `admin access follows a switch of server and a sign-in to another`() =
        runTest(dispatcher) {
            val servers =
                FakeServerRepository(
                    listOf(
                        Fixtures.server(id = "admin-server", isActive = true),
                        Fixtures.server(
                            id = "viewer-server",
                            isActive = false,
                            state = SessionState.Authenticated(Fixtures.user(isAdmin = false)),
                        ),
                    ),
                )
            val model = viewModel(servers)
            testScheduler.advanceUntilIdle()
            assertTrue("the active server's admin is an admin", model.state.value.isAdmin)

            servers.selectServer("viewer-server")
            testScheduler.advanceUntilIdle()
            assertFalse("a viewer on the server switched to is not", model.state.value.isAdmin)

            val added = servers.addServer("https://other.test", "Other")
            testScheduler.advanceUntilIdle()
            assertFalse("a server not yet signed in to grants nothing", model.state.value.isAdmin)

            servers.completeLogin(added.id, "cookie")
            testScheduler.advanceUntilIdle()
            assertTrue("signing in there as an admin grants it", model.state.value.isAdmin)
        }

    @Test
    fun `a stored session starts signed in`() =
        runTest(dispatcher) {
            // What the core restores a stored session as, with no request.
            val model = viewModel(FakeServerRepository(listOf(Fixtures.server(isActive = true))))
            testScheduler.advanceUntilIdle()

            val state = model.state.value
            assertTrue("the restore has answered", state.isReady)
            assertTrue("a cold start lands signed in", state.isSignedIn)
            assertFalse(state.isSessionExpired)
        }

    @Test
    fun `a restore that fails is ready and signed out`() =
        runTest(dispatcher) {
            // Empty, because the real repository publishes the list only from
            // a restore that succeeded: one that throws publishes nothing.
            val servers =
                FakeServerRepository(emptyList()).apply {
                    failWith = BeamException.Storage("the keystore is locked")
                }
            val model = viewModel(servers)
            testScheduler.advanceUntilIdle()

            val state = model.state.value
            assertTrue("the splash screen is not held forever", state.isReady)
            assertFalse("nothing restored is nothing signed in", state.isSignedIn)
        }

    @Test
    fun `a session expiring mid-use returns to sign-in`() =
        runTest(dispatcher) {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)))
            val model = viewModel(servers)
            testScheduler.advanceUntilIdle()
            assertTrue(model.state.value.isSignedIn)

            servers.expire("home")
            testScheduler.advanceUntilIdle()
            assertFalse("a refused session is not a usable one", model.state.value.isSignedIn)
            assertTrue("and the shell is told to send the viewer to sign in", model.state.value.isSessionExpired)

            servers.completeLogin("home", "cookie")
            testScheduler.advanceUntilIdle()
            assertTrue("signing in again restores it", model.state.value.isSignedIn)
            assertFalse(model.state.value.isSessionExpired)
        }
}
