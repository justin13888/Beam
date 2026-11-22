package dev.beam.android.feature.auth

import dev.beam.android.core.testing.DeviceLoginEnding
import dev.beam.android.core.testing.FakeServerRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.UserSummary

/** Signing in by the device authorization grant: when it is offered, and its polling. */
@OptIn(ExperimentalCoroutinesApi::class)
class AuthDeviceLoginTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    // ─── Which sign-in comes first ───────────────────────────────────────────

    @Test
    fun `a phone keeps the browser as its default even when the server offers codes`() =
        runTest {
            // Issue #151: "the screens do not change". The fake would sign in
            // at once, so a device login started behind the browser shows up
            // as an awaited login and a signed-in state.
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = DeviceLoginEnding.Approved(ADA)
                }
            val viewModel = AuthViewModel(servers, PHONE, FakeDownloadRepository())
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.loginUrl)
            assertNull(viewModel.state.value.devicePrompt)
            assertEquals("nothing polls behind the browser", 0, servers.deviceLoginsAwaited)
            assertTrue(!viewModel.state.value.isSignedIn)
        }

    @Test
    fun `a phone can ask for a code instead, which replaces the browser`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = DeviceLoginEnding.Unanswered
                }
            val viewModel = AuthViewModel(servers, PHONE, FakeDownloadRepository())
            testScheduler.advanceUntilIdle()
            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            viewModel.signInWithCode()
            testScheduler.runCurrent()

            val prompted = viewModel.state.value
            assertEquals("BCDF-GHJK", prompted.devicePrompt?.userCode)
            assertNull("the code replaces the browser", prompted.loginUrl)
            assertEquals("and the core is waiting on it", 1, servers.deviceLoginsAwaited)
            viewModel.onDeviceLoginCancelled()
        }

    @Test
    fun `a phone signs in with a code once the core says it is approved`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = DeviceLoginEnding.Approved(ADA)
                }
            val viewModel = AuthViewModel(servers, PHONE, FakeDownloadRepository())
            testScheduler.advanceUntilIdle()
            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            viewModel.signInWithCode()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertTrue(state.isSignedIn)
            assertNull(state.devicePrompt)
            assertNull(state.loginUrl)
            assertEquals(1, servers.deviceLoginsAwaited)
        }

    @Test
    fun `asking for a code from a server without them leaves the browser open`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), PHONE, FakeDownloadRepository())
            testScheduler.advanceUntilIdle()
            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            viewModel.signInWithCode()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertNotNull("the viewer stays where they were", state.loginUrl)
            assertNull(state.devicePrompt)
            assertNotNull("and is told why nothing changed", state.error)
        }

    @Test
    fun `a device without a browser shows a code first`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = DeviceLoginEnding.Unanswered
                }
            val viewModel = AuthViewModel(servers, TV, FakeDownloadRepository())
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.runCurrent()

            val state = viewModel.state.value
            assertEquals("BCDF-GHJK", state.devicePrompt?.userCode)
            assertNull("the browser is the fallback, not the default", state.loginUrl)
            viewModel.onDeviceLoginCancelled()
        }

    @Test
    fun `cancelling a device login stops polling`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = DeviceLoginEnding.Unanswered
                }
            val viewModel = signingInWithoutABrowser(servers)
            assertEquals("the login was being awaited", 1, servers.deviceLoginsAwaited)

            viewModel.onDeviceLoginCancelled()
            testScheduler.runCurrent()

            // Cancelling the coroutine drops the core's future, which is what
            // stops its poll loop; that half is the core's own test.
            assertEquals("the core's wait was cancelled", 1, servers.deviceLoginsCancelled)
            assertNull("and the code is gone", viewModel.state.value.devicePrompt)
            assertNull("giving up is not a failure", viewModel.state.value.error)
        }

    @Test
    fun `a device without a browser falls back to it when the server has no codes`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), TV, FakeDownloadRepository())
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.loginUrl)
            assertNull(viewModel.state.value.devicePrompt)
            assertNull("falling back is not a failure", viewModel.state.value.error)
        }

    // ─── Waiting ──────────────────────────────────────────────────────────────
    //
    // The pacing between polls -- interval, slow_down, Retry-After, polling
    // through a retryable failure -- is the core's `await_device_login`, and
    // its tests are in beam-client-core. Here: what the screen does with the
    // wait's outcome.

    @Test
    fun `an approved device login signs in and clears the code`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = DeviceLoginEnding.Approved(ADA)
                }
            val viewModel = signingInWithoutABrowser(servers)
            testScheduler.advanceUntilIdle()

            assertEquals(1, servers.deviceLoginsAwaited)
            assertTrue(viewModel.state.value.isSignedIn)
            assertNull(viewModel.state.value.devicePrompt)
            assertNull(viewModel.state.value.error)
        }

    @Test
    fun `a refused, expired or unknown device login ends with an error the viewer can read`() =
        runTest {
            for (ending in listOf(
                BeamException.Forbidden("refused", DENIED),
                BeamException.Server(410u, false, "expired", EXPIRED),
                BeamException.BadRequest("unknown", INVALID),
            )) {
                val servers =
                    FakeServerRepository().apply {
                        deviceLogin = DeviceLoginEnding.Fails(ending)
                    }
                val viewModel = signingInWithoutABrowser(servers)
                testScheduler.advanceUntilIdle()

                assertNull("$ending clears the code", viewModel.state.value.devicePrompt)
                assertNotNull(viewModel.state.value.error)
                assertTrue(!viewModel.state.value.isSignedIn)
            }
        }

    /** Connects on a device with no browser, the core's wait begun. */
    private fun TestScope.signingInWithoutABrowser(servers: FakeServerRepository): AuthViewModel {
        val viewModel = AuthViewModel(servers, TV, FakeDownloadRepository())
        testScheduler.advanceUntilIdle()
        viewModel.onAddressChange("beam.example.com")
        viewModel.connect()
        testScheduler.runCurrent()
        return viewModel
    }

    private companion object {
        const val DENIED = "https://beam.justinchung.net/reference/errors/#device-login-denied"
        const val EXPIRED = "https://beam.justinchung.net/reference/errors/#device-login-expired"
        const val INVALID = "https://beam.justinchung.net/reference/errors/#device-login-invalid"
        val PHONE = BrowserAvailability { true }
        val TV = BrowserAvailability { false }
        val ADA = UserSummary("u-1", "Ada", null, false, null)
    }
}
