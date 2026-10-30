package dev.beam.android.feature.auth

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
import uniffi.beam_client_core.DeviceLoginStep
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
            // Issue #151: "the screens do not change". The script would sign
            // in on the first poll, so a device login started behind the
            // browser shows up as a poll and a signed-in state.
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = listOf(DeviceLoginStep.SignedIn(ADA))
                }
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.loginUrl)
            assertNull(viewModel.state.value.devicePrompt)
            assertEquals("nothing polls behind the browser", 0, servers.devicePolls)
            assertTrue(!viewModel.state.value.isSignedIn)
        }

    @Test
    fun `a phone can sign in with a code instead`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = listOf(DeviceLoginStep.SignedIn(ADA))
                }
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()
            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            viewModel.signInWithCode()
            testScheduler.runCurrent()

            val prompted = viewModel.state.value
            assertEquals("BCDF-GHJK", prompted.devicePrompt?.userCode)
            assertNull("the code replaces the browser", prompted.loginUrl)

            testScheduler.advanceUntilIdle()
            assertTrue(viewModel.state.value.isSignedIn)
            assertEquals(1, servers.devicePolls)
        }

    @Test
    fun `asking for a code from a server without them leaves the browser open`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), PHONE)
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
                    deviceLogin = listOf(DeviceLoginStep.Waiting(5u, false))
                }
            val viewModel = AuthViewModel(servers, TV)
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
                    deviceLogin = listOf(DeviceLoginStep.Waiting(5u, false))
                }
            val viewModel = AuthViewModel(servers, TV)
            testScheduler.advanceUntilIdle()
            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.runCurrent()
            testScheduler.advanceTimeBy(5_001)
            assertEquals("the login was being polled", 1, servers.devicePolls)

            viewModel.onDeviceLoginCancelled()
            // Long enough for many more polls, had the loop survived.
            testScheduler.advanceTimeBy(60_000)

            assertEquals("no poll after the viewer gave up", 1, servers.devicePolls)
            assertNull("and the code is gone", viewModel.state.value.devicePrompt)
        }

    @Test
    fun `a device without a browser falls back to it when the server has no codes`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), TV)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.loginUrl)
            assertNull(viewModel.state.value.devicePrompt)
            assertNull("falling back is not a failure", viewModel.state.value.error)
        }

    // ─── Polling ──────────────────────────────────────────────────────────────

    @Test
    fun `polling waits the interval and signs in on approval`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin =
                        listOf(
                            DeviceLoginStep.Waiting(10u, true),
                            DeviceLoginStep.SignedIn(ADA),
                        )
                }
            val viewModel = signingInWithoutABrowser(servers)

            testScheduler.advanceTimeBy(5_001)
            assertEquals("the first poll waits the prompt's interval", 1, servers.devicePolls)
            testScheduler.advanceTimeBy(9_000)
            assertEquals("a slow_down stretches the next wait", 1, servers.devicePolls)
            testScheduler.advanceTimeBy(1_001)
            assertEquals(2, servers.devicePolls)

            assertTrue(viewModel.state.value.isSignedIn)
            assertNull(viewModel.state.value.devicePrompt)
        }

    @Test
    fun `a poll that fails in a way a later one could not keeps the login going`() =
        runTest {
            // The viewer may be halfway through approving on their phone; a
            // server hiccup or a dropped connection must not throw that away.
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = listOf(DeviceLoginStep.SignedIn(ADA))
                    devicePollFailures.addAll(
                        listOf(
                            BeamException.Server(503u, true, "unavailable", OIDC_UNAVAILABLE),
                            BeamException.Network("connection reset", retryable = true),
                        ),
                    )
                }
            val viewModel = signingInWithoutABrowser(servers)

            testScheduler.advanceTimeBy(5_001)
            assertEquals(1, servers.devicePolls)
            assertNotNull("still waiting", viewModel.state.value.devicePrompt)
            assertNull(viewModel.state.value.error)
            testScheduler.advanceTimeBy(5_000)
            assertEquals("retried at the same interval", 2, servers.devicePolls)
            assertNotNull(viewModel.state.value.devicePrompt)
            testScheduler.advanceTimeBy(5_000)

            assertEquals(3, servers.devicePolls)
            assertTrue(viewModel.state.value.isSignedIn)
        }

    @Test
    fun `a rate-limited poll waits as long as the server asks`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    deviceLogin = listOf(DeviceLoginStep.SignedIn(ADA))
                    devicePollFailures.add(BeamException.RateLimited(30uL))
                }
            val viewModel = signingInWithoutABrowser(servers)

            testScheduler.advanceTimeBy(5_001)
            assertEquals(1, servers.devicePolls)
            testScheduler.advanceTimeBy(29_000)
            assertEquals("not before Retry-After", 1, servers.devicePolls)
            testScheduler.advanceTimeBy(1_000)
            assertEquals(2, servers.devicePolls)
            assertTrue(viewModel.state.value.isSignedIn)
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
                        deviceLogin = listOf(DeviceLoginStep.Waiting(5u, false))
                        devicePollFailures.add(ending)
                    }
                val viewModel = signingInWithoutABrowser(servers)

                testScheduler.advanceTimeBy(60_000)

                assertEquals("$ending ends the polling", 1, servers.devicePolls)
                assertNull(viewModel.state.value.devicePrompt)
                assertNotNull(viewModel.state.value.error)
                assertTrue(!viewModel.state.value.isSignedIn)
            }
        }

    /** Connects on a device with no browser, leaving the first poll pending. */
    private fun TestScope.signingInWithoutABrowser(servers: FakeServerRepository): AuthViewModel {
        val viewModel = AuthViewModel(servers, TV)
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
        const val OIDC_UNAVAILABLE = "https://beam.justinchung.net/reference/errors/#oidc-unavailable"
        val PHONE = BrowserAvailability { true }
        val TV = BrowserAvailability { false }
        val ADA = UserSummary("u-1", "Ada", null, false, null)
    }
}
