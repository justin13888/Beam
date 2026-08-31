package dev.beam.android.feature.auth

import app.cash.turbine.test
import dev.beam.android.core.testing.FakeServerRepository
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
            val servers = FakeServerRepository()
            val viewModel = AuthViewModel(servers)

            viewModel.state.test {
                skipItems(1)
                assertTrue(awaitItem().knownServers.isNotEmpty())
            }
        }

    @Test
    fun `connecting is refused while the address is blank`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository())
            testScheduler.advanceUntilIdle()

            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNull(viewModel.state.value.loginUrl)
        }

    @Test
    fun `a reachable server yields a sign-in url`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository())
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
            val servers =
                FakeServerRepository().apply {
                    failOnce = BeamException.UntrustedCertificate("beam.local", certificate())
                }
            val viewModel = AuthViewModel(servers)
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
            val servers =
                FakeServerRepository().apply {
                    failOnce = BeamException.UntrustedCertificate("beam.local", certificate())
                }
            val viewModel = AuthViewModel(servers)
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
            val servers =
                FakeServerRepository().apply {
                    failOnce = BeamException.UntrustedCertificate("beam.local", certificate())
                }
            val viewModel = AuthViewModel(servers)
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
            val servers =
                FakeServerRepository().apply {
                    failWith = BeamException.Network("could not reach the server", retryable = true)
                }
            val viewModel = AuthViewModel(servers)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("nowhere.invalid")
            viewModel.connect()
            testScheduler.advanceUntilIdle()

            assertNotNull(viewModel.state.value.error)
            assertNull(viewModel.state.value.pendingTrust)
        }

    @Test
    fun `a session cookie completes sign-in`() =
        runTest {
            val servers = FakeServerRepository()
            val viewModel = AuthViewModel(servers)
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
            val viewModel = AuthViewModel(servers)
            testScheduler.advanceUntilIdle()

            viewModel.onSessionCookie("stray")
            testScheduler.advanceUntilIdle()

            assertNull(servers.capturedCookie)
        }

    @Test
    fun `cancelling sign-in closes the browser without an error`() =
        runTest {
            val viewModel = AuthViewModel(FakeServerRepository())
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
                    failWith = BeamException.Network("unreachable", retryable = true)
                }
            val viewModel = AuthViewModel(servers)
            testScheduler.advanceUntilIdle()

            viewModel.onAddressChange("nowhere.invalid")
            viewModel.connect()
            testScheduler.advanceUntilIdle()
            viewModel.onAddressChange("beam.example.com")

            assertNull(viewModel.state.value.error)
        }

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
    }
}
