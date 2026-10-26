package dev.beam.android

import android.app.Application
import androidx.test.core.app.ApplicationProvider
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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.beam_client_core.SessionState

/**
 * Admin access follows the active server: it is per server, so a switch in
 * settings or a sign-in to another server changes it without a restart.
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

    private fun viewModel(servers: FakeServerRepository) =
        MainViewModel(
            servers = servers,
            playback = FakePlaybackRepository(),
            preferences = FakePreferencesRepository(),
            context = ApplicationProvider.getApplicationContext(),
        )

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
}
