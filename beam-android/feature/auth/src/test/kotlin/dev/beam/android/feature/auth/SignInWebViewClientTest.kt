package dev.beam.android.feature.auth

import android.net.http.SslCertificate
import android.net.http.SslError
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.util.Base64

/**
 * The sign-in browser's certificate decision, with the core's rule faked: what
 * is asserted is that the client asks the rule about the right certificate for
 * the right host, and does what it answers. The rule itself is the core's, and
 * is tested there.
 */
@RunWith(RobolectricTestRunner::class)
class SignInWebViewClientTest {
    @Test
    fun `a certificate the core admits for the server proceeds`() {
        val decision = RecordingDecision(accepts = true)
        val client = client(decision)

        val outcome = outcomeOf(client, sslError("https://beam.test/v1/auth/login"))

        assertEquals(Outcome.PROCEED, outcome)
        val asked = decision.asked.single()
        assertArrayEquals("the presented certificate, encoded", certificate.encoded, asked.der)
        assertEquals("beam.test", asked.host)
        assertEquals(listOf(PIN), asked.trustedFingerprints)
        assertEquals("the time the rule judges validity at", NOW, asked.nowUnix)
    }

    @Test
    fun `a certificate the core refuses is cancelled`() {
        val client = client(RecordingDecision(accepts = false))

        assertEquals(Outcome.CANCEL, outcomeOf(client, sslError("https://beam.test/v1/auth/login")))
    }

    @Test
    fun `another host is cancelled without asking, however the core would answer`() {
        // The pins were recorded for the server; the identity provider a
        // redirect leads to is not covered by them.
        val decision = RecordingDecision(accepts = true)
        val client = client(decision)

        val outcome = outcomeOf(client, sslError("https://idp.example.com/authorize"))

        assertEquals(Outcome.CANCEL, outcome)
        assertTrue(decision.asked.isEmpty())
    }

    @Test
    fun `the host is compared as both sides spell it`() {
        val decision = RecordingDecision(accepts = true)
        val client = client(decision)

        assertEquals(Outcome.PROCEED, outcomeOf(client, sslError("https://BEAM.test./v1/auth/login")))
        assertEquals("beam.test", decision.asked.single().host)
    }

    @Test
    fun `with nothing pinned every error is cancelled without asking`() {
        val decision = RecordingDecision(accepts = true)
        val client = SignInWebViewClient(SIGN_IN_URL, emptyList(), {}, decision, { NOW })

        assertEquals(Outcome.CANCEL, outcomeOf(client, sslError("https://beam.test/v1/auth/login")))
        assertTrue(decision.asked.isEmpty())
    }

    @Test
    fun `an error with nothing to judge is cancelled`() {
        assertEquals(Outcome.CANCEL, outcomeOf(client(RecordingDecision(accepts = true)), null))
    }

    private fun client(decision: PinnedCertificateDecision) =
        SignInWebViewClient(SIGN_IN_URL, listOf(PIN), {}, decision, { NOW })

    private fun sslError(url: String) = SslError(SslError.SSL_UNTRUSTED, SslCertificate(certificate), url)

    /** Which of the two the client called; it must call exactly one. */
    private fun outcomeOf(
        client: SignInWebViewClient,
        error: SslError?,
    ): Outcome {
        val calls = mutableListOf<Outcome>()
        client.decide(error, proceed = { calls += Outcome.PROCEED }, cancel = { calls += Outcome.CANCEL })
        return calls.single()
    }

    private enum class Outcome { PROCEED, CANCEL }

    private class RecordingDecision(
        private val accepts: Boolean,
    ) : PinnedCertificateDecision {
        data class Asked(
            val der: ByteArray,
            val host: String,
            val trustedFingerprints: List<String>,
            val nowUnix: Long,
        )

        val asked = mutableListOf<Asked>()

        override fun accepts(
            der: ByteArray,
            host: String,
            trustedFingerprints: List<String>,
            nowUnix: Long,
        ): Boolean {
            asked += Asked(der, host, trustedFingerprints, nowUnix)
            return accepts
        }
    }

    private companion object {
        const val SIGN_IN_URL = "https://beam.test/v1/auth/login?redirect=%2F"
        const val PIN = "AA:BB"
        const val NOW = 1_800_000_000L

        /** A self-signed certificate for `beam.test`; its content matters only as bytes. */
        val certificate: X509Certificate =
            CertificateFactory
                .getInstance("X.509")
                .generateCertificate(
                    Base64
                        .getMimeDecoder()
                        .decode(
                            """
                            MIIBczCCARqgAwIBAgIUdjv4UOSlq1xzhVXvQeyS1i++I+kwCgYIKoZIzj0EAwIw
                            FDESMBAGA1UEAwwJYmVhbS50ZXN0MCAXDTI2MTEyMTEwNDMxNFoYDzIxMjYxMDI4
                            MTA0MzE0WjAUMRIwEAYDVQQDDAliZWFtLnRlc3QwWTATBgcqhkjOPQIBBggqhkjO
                            PQMBBwNCAARTGey7r1CLf6MP0Bg5pqSYmIWxZngQkU6WMIgm3oogLbIhjrTCmzV4
                            V+5KRSfSWYUA7xMSRdZOUmcOIgyQdZOeo0gwRjAdBgNVHQ4EFgQUrZRqMz+jtO9X
                            JJBUrHYDJhmoiAYwDwYDVR0TAQH/BAUwAwEB/zAUBgNVHREEDTALggliZWFtLnRl
                            c3QwCgYIKoZIzj0EAwIDRwAwRAIgX5hBJM5l6sfntk8FjhY2Dq7zHw46P+YwOWhq
                            f2P7y5wCIBw63Xe/m9mRzLFvYj9FAO4dHqVxj2FVzQ5juhXwVDqv
                            """.trimIndent(),
                        ).inputStream(),
                ) as X509Certificate
    }
}
