package dev.beam.android.feature.auth

import app.cash.turbine.test
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
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.CertificateDetails

@OptIn(ExperimentalCoroutinesApi::class)
class AuthViewModelTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() {
        Dispatchers.setMain(dispatcher)
    }

    @After
    fun tearDown() {
        Dispatchers.resetMain()
    }

    @Test
    fun `known servers are offered once they have been restored`() =
        runTest {
            val servers =
                FakeServerRepository(
                    stored =
                        listOf(
                            Fixtures.server(id = "cabin", displayName = "Cabin", isActive = false),
                            Fixtures.server(id = "home", displayName = "Home", isActive = true),
                        ),
                )
            val viewModel = AuthViewModel(servers, PHONE)

            viewModel.state.test {
                val beforeRestore = awaitItem().knownServers.map { it.id }
                assertEquals("nothing is offered before the registry is read", emptyList<String>(), beforeRestore)
                assertEquals(listOf("cabin", "home"), awaitItem().knownServers.map { it.id })
            }
        }

    @Test
    fun `adding the same address twice lists one server`() =
        runTest {
            // The core names a server by its origin, so the same server typed
            // twice -- once with its scheme and a trailing slash -- is one
            // entry the second connect selects, not a second shortcut.
            val servers = FakeServerRepository()
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            val first = viewModel.state.value.serverId
            viewModel.onSignInCancelled()
            viewModel.onAddressChange("https://beam.example.com/")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertEquals(first, viewModel.state.value.serverId)
            assertEquals(listOf(first), servers.restore().map { it.id })
        }

    @Test
    fun `connecting is refused while the address is blank`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNull(viewModel.state.value.loginUrl)
        }

    @Test
    fun `a reachable server yields a sign-in url`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.loginUrl)
            assertNotNull(viewModel.state.value.serverId)
        }

    @Test
    fun `an untrusted certificate becomes a question rather than an error`() =
        runTest {
            // The distinction matters: an error is something the app failed at,
            // and a question is something only the viewer can answer. Rendering
            // this as an error would leave them with no way to proceed.
            //
            // Raised by starting a device login, the first call of a connect
            // that reaches the server: adding a server and building its
            // sign-in address are answered by the core without a handshake.
            val servers = FakeServerRepository().apply { failOnce = untrustedCertificate() }
            val viewModel = AuthViewModel(servers, TV)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.local")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertNotNull(state.pendingTrust)
            assertNull("a trust question must not also read as a failure", state.error)
            assertEquals("beam.local", state.pendingTrust!!.host)
        }

    @Test
    fun `accepting a certificate records it and retries the connection`() =
        runTest {
            val servers = FakeServerRepository().apply { failOnce = untrustedCertificate() }
            val viewModel = AuthViewModel(servers, TV)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.local")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            val trust = viewModel.state.value.pendingTrust!!
            viewModel.acceptCertificate(trust)
            testScheduler.advanceUntilIdle()

            assertTrue(
                servers.trusted.values
                    .flatten()
                    .contains(FINGERPRINT),
            )
            assertNotNull(
                "accepting must retry, not send the viewer back to the address field",
                viewModel.state.value.loginUrl,
            )
            assertNull(viewModel.state.value.pendingTrust)
        }

    @Test
    fun `declining a certificate explains why nothing happened`() =
        runTest {
            val servers = FakeServerRepository().apply { failOnce = untrustedCertificate() }
            val viewModel = AuthViewModel(servers, TV)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.local")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            viewModel.declineCertificate()

            val state = viewModel.state.value
            assertNull(state.pendingTrust)
            assertNotNull("a silent no-op would look like a broken button", state.error)
            assertTrue(servers.trusted.isEmpty())
        }

    @Test
    fun `a network failure is reported as an error the viewer can read`() =
        runTest {
            // Through the first call that reaches the server. The core adds a
            // server without contacting it, so an unreachable one is found out
            // only once something is asked of it.
            val servers =
                FakeServerRepository().apply {
                    failOnce = BeamException.Network("could not reach the server", retryable = true)
                }
            val viewModel = AuthViewModel(servers, TV)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("nowhere.invalid")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.error)
            assertNull(viewModel.state.value.pendingTrust)
            assertNull("no code was issued", viewModel.state.value.devicePrompt)
        }

    @Test
    fun `a cookie the server refuses is reported and closes the browser`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    failOnce = BeamException.Unauthenticated()
                }
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            viewModel.onSessionCookie("refused-session-value")
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertNotNull(state.error)
            assertTrue(!state.isSignedIn)
            assertNull("the browser closes on the refusal too", state.loginUrl)
        }

    @Test
    fun `a session cookie completes sign-in`() =
        runTest {
            val servers = FakeServerRepository()
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            viewModel.onSessionCookie("opaque-session-value")
            testScheduler.advanceUntilIdle()

            assertTrue(viewModel.state.value.isSignedIn)
            assertEquals("opaque-session-value", servers.capturedCookie)
            assertNull(
                "the browser must close once the cookie is captured",
                viewModel.state.value.loginUrl,
            )
        }

    @Test
    fun `a cookie arriving with no server selected is ignored`() =
        runTest {
            // The WebView reports every completed navigation, and a stale one can
            // arrive after the flow has been abandoned.
            val servers = FakeServerRepository()
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onSessionCookie("stray")
            testScheduler.advanceUntilIdle()

            assertNull(servers.capturedCookie)
        }

    @Test
    fun `cancelling sign-in closes the browser without an error`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository(), PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("beam.example.com")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            viewModel.onSignInCancelled()

            assertNull(viewModel.state.value.loginUrl)
            assertNull("cancelling is a choice, not a failure", viewModel.state.value.error)
        }

    @Test
    fun `typing clears a previous error`() =
        runTest {
            val servers =
                FakeServerRepository().apply {
                    failOnce = BeamException.Network("unreachable", retryable = true)
                }
            val viewModel = AuthViewModel(servers, TV)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("nowhere.invalid")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            assertNotNull("the connect failed", viewModel.state.value.error)
            viewModel.onAddressChange("beam.example.com")

            assertNull(viewModel.state.value.error)
        }

    @Test
    fun `forgetting a server removes it from the shortcuts`() =
        runTest {
            val servers =
                FakeServerRepository(
                    listOf(
                        Fixtures.server(id = "home", isActive = true),
                        Fixtures.server(id = "cabin", isActive = false),
                    ),
                )
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.forget("cabin")
            testScheduler.advanceUntilIdle()

            assertEquals(
                listOf("home"),
                viewModel.state.value.knownServers
                    .map { it.id },
            )
        }

    @Test
    fun `connecting to a known server signs in to it without adding another`() =
        runTest {
            val servers =
                FakeServerRepository(
                    listOf(
                        Fixtures.server(id = "home", isActive = true),
                        Fixtures.server(id = "cabin", isActive = false),
                    ),
                )
            val viewModel = AuthViewModel(servers, PHONE)
            testScheduler.advanceUntilIdle()

            viewModel.connect(existingServerId = "cabin")
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals("cabin", state.serverId)
            assertNotNull(state.loginUrl)
            val known = servers.restore()
            assertEquals("no second entry", 2, known.size)
            assertEquals("cabin", known.single { it.isActive }.id)
        }

    private fun untrustedCertificate() = BeamException.UntrustedCertificate("beam.local", certificate())

    private fun certificate() =
        CertificateDetails(
            sha256Fingerprint = FINGERPRINT,
            spkiSha256Base64 = "c3BraQ==",
            subject = "CN=beam.local",
            issuer = "CN=beam.local",
            notBeforeUnix = 0L,
            notAfterUnix = Long.MAX_VALUE,
            subjectAltNames = listOf("beam.local"),
            serialHex = "01",
            isSelfSigned = true,
            isExpired = false,
        )

    private companion object {
        const val FINGERPRINT = "AA:BB:CC:DD"
        val PHONE = BrowserAvailability { true }
        val TV = BrowserAvailability { false }
    }
}
