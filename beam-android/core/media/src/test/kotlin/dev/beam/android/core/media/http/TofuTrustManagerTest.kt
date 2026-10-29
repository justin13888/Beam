package dev.beam.android.core.media.http

import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import okhttp3.HttpUrl
import okhttp3.Request
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.net.InetAddress
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.X509TrustManager

class TofuTrustManagerTest {
    @Test
    fun `a certificate the platform accepts is accepted without consulting pins`() {
        val platform = FakeTrustManager(accepts = true)
        val pins = RecordingPinPolicy(accepts = false)
        val manager = TofuTrustManager(platform, listOf(PINNED_FINGERPRINT), PINNED_HOST, pins)

        manager.checkServerTrusted(arrayOf(certificate()), "RSA", engineFor(PINNED_HOST))

        assertTrue("the platform path must be tried first", platform.wasConsulted)
        assertTrue("a pin can only widen trust, never be consulted to narrow it", pins.asked.isEmpty())
    }

    @Test
    fun `the core decides a pin from the leaf, the connected host and the time`() {
        // The case the whole class exists for: a self-signed certificate on a
        // LAN server that the user was shown and agreed to. Whether it is
        // accepted is the core's rule; this is what the core is asked.
        val pins = RecordingPinPolicy(accepts = true)
        val manager =
            TofuTrustManager(
                FakeTrustManager(accepts = false),
                listOf(PINNED_FINGERPRINT),
                PINNED_HOST,
                pins,
                nowUnix = { NOW },
            )

        manager.checkServerTrusted(arrayOf(certificate()), "RSA", engineFor("BEAM.local"))

        val asked = pins.asked.single()
        assertArrayEquals("the leaf as presented", certificate().encoded, asked.der)
        assertEquals("the host connected to", PINNED_HOST, asked.host)
        assertEquals(listOf(PINNED_FINGERPRINT), asked.trustedFingerprints)
        assertEquals(NOW, asked.nowUnix)
    }

    @Test
    fun `a pin the core refuses leaves the platform's verdict standing`() {
        // An expired, wrong-host or unpinned certificate: the core says no,
        // and the platform's own diagnostic is what surfaces.
        val platform = FakeTrustManager(accepts = false)
        val manager = TofuTrustManager(platform, listOf(PINNED_FINGERPRINT), PINNED_HOST, RecordingPinPolicy(false))

        val failure = runCatching { manager.checkServerTrusted(arrayOf(certificate()), "RSA", engineFor(PINNED_HOST)) }

        assertSame(platform.failure, failure.exceptionOrNull())
    }

    @Test
    fun `a pin is honoured only for its host`() {
        // Over a real handshake, because the host comes from the connection:
        // one certificate, valid for two names, served on both. The pin was
        // recorded for one of them, so reaching the server by the other --
        // as a redirect off the server's origin would -- meets the platform's
        // verdict alone, and the core is never asked.
        val certificate =
            HeldCertificate
                .Builder()
                .addSubjectAlternativeName(SERVER_HOST)
                .addSubjectAlternativeName(OTHER_HOST)
                .build()
        val pins = RecordingPinPolicy(accepts = true)
        MockWebServer().use { server ->
            server.useHttps(
                HandshakeCertificates
                    .Builder()
                    .heldCertificate(certificate)
                    .build()
                    .sslSocketFactory(),
            )
            server.enqueue(MockResponse.Builder().body("ok").build())
            server.start(InetAddress.getByName(SERVER_HOST), 0)
            val client =
                BeamHttpClientFactory(BeamHttpClientFactory.shared(), pins)
                    .trusting(listOf("pinned"), host = SERVER_HOST)

            val pinned = client.newCall(Request.Builder().url(server.url("/", SERVER_HOST)).build()).execute()
            assertEquals(200, pinned.use { it.code })
            assertEquals(listOf(SERVER_HOST), pins.asked.map { it.host })

            val elsewhere =
                runCatching { client.newCall(Request.Builder().url(server.url("/", OTHER_HOST)).build()).execute() }
            assertTrue(
                "another host is refused, got $elsewhere",
                elsewhere.exceptionOrNull() is IOException,
            )
            assertEquals("the core is not asked about another host", 1, pins.asked.size)
        }
    }

    @Test
    fun `without a connection the pin is held to the host it was recorded for`() {
        val pins = RecordingPinPolicy(accepts = true)
        val manager = TofuTrustManager(FakeTrustManager(accepts = false), listOf(PINNED_FINGERPRINT), PINNED_HOST, pins)

        manager.checkServerTrusted(arrayOf(certificate()), "RSA")

        assertEquals(PINNED_HOST, pins.asked.single().host)
    }

    @Test
    fun `no pinned host means no pin is honoured`() {
        val pins = RecordingPinPolicy(accepts = true)
        val manager = TofuTrustManager(FakeTrustManager(accepts = false), listOf(PINNED_FINGERPRINT), "", pins)

        val failure = runCatching { manager.checkServerTrusted(arrayOf(certificate()), "RSA", engineFor(PINNED_HOST)) }

        assertTrue(failure.exceptionOrNull() is CertificateException)
        assertTrue(pins.asked.isEmpty())
    }

    @Test
    fun `a host is compared as both sides spell it`() {
        // The core reports an IPv6 server as `[::1]`; the TLS stack says `::1`.
        for ((pinned, connected) in listOf(
            "[::1]" to "::1",
            "Beam.Local" to "beam.local",
            "beam.local." to "beam.local",
        )) {
            assertEquals(
                "$pinned against $connected",
                TofuTrustManager.canonicalHost(pinned),
                TofuTrustManager.canonicalHost(connected),
            )
        }
        assertFalse(TofuTrustManager.canonicalHost("beam.local") == TofuTrustManager.canonicalHost("beam.locale"))
    }

    @Test
    fun `an empty chain is rejected rather than treated as trusted`() {
        val manager =
            TofuTrustManager(FakeTrustManager(accepts = true), emptyList(), PINNED_HOST, RecordingPinPolicy(true))

        val failure = runCatching { manager.checkServerTrusted(emptyArray(), "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
    }

    @Test
    fun `a null chain is rejected`() {
        val manager =
            TofuTrustManager(FakeTrustManager(accepts = true), emptyList(), PINNED_HOST, RecordingPinPolicy(true))

        val failure = runCatching { manager.checkServerTrusted(null, "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
    }

    @Test
    fun `client trust is delegated untouched`() {
        // The exception this class makes is for *servers*. Widening client
        // trust would be a different decision entirely, and nobody asked for
        // it.
        val manager =
            TofuTrustManager(
                FakeTrustManager(accepts = false),
                listOf(PINNED_FINGERPRINT),
                PINNED_HOST,
                RecordingPinPolicy(true),
            )

        val failure = runCatching { manager.checkClientTrusted(arrayOf(certificate()), "RSA", engineFor(PINNED_HOST)) }

        assertTrue(
            "a server pin must never admit a client certificate",
            failure.exceptionOrNull() is CertificateException,
        )
    }

    private fun certificate(): X509Certificate =
        java.security.cert.CertificateFactory
            .getInstance("X.509")
            .generateCertificate(SELF_SIGNED_PEM.byteInputStream()) as X509Certificate

    /** An engine for a connection to [host], as the TLS stack hands one over. */
    private fun engineFor(host: String) = SSLContext.getDefault().createSSLEngine(host, 443)

    /** A server URL that names its host as given, rather than by reverse lookup. */
    private fun MockWebServer.url(
        path: String,
        host: String,
    ): HttpUrl =
        url(path)
            .newBuilder()
            .host(host)
            .build()

    private class FakeTrustManager(
        private val accepts: Boolean,
    ) : X509TrustManager {
        val failure = CertificateException("platform rejected the chain")

        var wasConsulted: Boolean = false
            private set

        override fun checkServerTrusted(
            chain: Array<out X509Certificate>?,
            authType: String?,
        ) {
            wasConsulted = true
            if (!accepts) throw failure
        }

        override fun checkClientTrusted(
            chain: Array<out X509Certificate>?,
            authType: String?,
        ) {
            if (!accepts) throw failure
        }

        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    private companion object {
        const val PINNED_HOST = "beam.local"
        const val NOW = 1_800_000_000L
        const val SERVER_HOST = "localhost"
        const val OTHER_HOST = "127.0.0.1"

        /**
         * `openssl req -x509 -newkey rsa:2048 -subj /CN=beam.local
         *  -addext subjectAltName=DNS:beam.local`
         */
        const val PINNED_FINGERPRINT: String =
            "F8:4E:00:C5:37:BA:E1:E6:C1:B1:8E:61:18:D4:D1:E2:" +
                "C8:4F:F5:12:04:9C:67:9C:AE:9E:27:71:04:2B:B2:41"

        val SELF_SIGNED_PEM: String =
            """
            -----BEGIN CERTIFICATE-----
            MIIDIjCCAgqgAwIBAgIUVQuWvYfanRsE0j3CJu80rZxzwVswDQYJKoZIhvcNAQEL
            BQAwFTETMBEGA1UEAwwKYmVhbS5sb2NhbDAeFw0yNjA4MzEwMjI3MzVaFw0zNjA4
            MjgwMjI3MzVaMBUxEzARBgNVBAMMCmJlYW0ubG9jYWwwggEiMA0GCSqGSIb3DQEB
            AQUAA4IBDwAwggEKAoIBAQCeJ9uiKZ/fcTcYGsQkZqO9Oqqt/aXR4fD5hAAm4uLU
            0wswdU1pBQRrX8CxL565/k1Mn86PacINR4uObKUhpvT9xq3qlFZ/ZFZt3Nph5x9V
            3MrW6itIUQDuxXygma6vzkljtJncS0QmoCFiw+OxEcPf8ObvqnCP1uD/lTBiBYG8
            zvjskeZiwAxFAqoj98F8y+P3xlsjB2iFtnd27WqJxCD/j5ogBq0a21O82MFmtBtQ
            G7NWjHd2kUpzD2A3C7AMnIZsrKOzPzGNy0nLRk1dGNKRD1snO0UDIOte+uSxwhz+
            bbny/j9PLvw7LrPpY2mDM3uOvtv0e8YN1CQtzqwBCC1NAgMBAAGjajBoMB0GA1Ud
            DgQWBBSU0YdxuUvmnzsMRkzd/ZGzAXYqPDAfBgNVHSMEGDAWgBSU0YdxuUvmnzsM
            Rkzd/ZGzAXYqPDAPBgNVHRMBAf8EBTADAQH/MBUGA1UdEQQOMAyCCmJlYW0ubG9j
            YWwwDQYJKoZIhvcNAQELBQADggEBABOK2PcCElEH1eIIU14L4NPFPmJHhw4WdPe7
            jcNk1x9QoNp2g7yV5bZfzAAB8a/PmCeAuxnVEBkiZELW5zwqSz1CvmuQmsl3P/QE
            lvWex4qWNayj4eaqDv+jng2sVZIa7GZsJj8iO8uRrBXPVqRqUxGRrXTqIQ2cAx7i
            0e/wXcbSfbzP4m+pZIB3+6PgpIwkEu4cFqrdkVP+BoeakIrdasIOmH2rZMyv670i
            dR8/luRmQtDJx6Q0SQ90ITWCP0yg3xwk5FLC24rxr6tquJ/Rp6vBhb7G3ZE7VtV8
            qmrbJhmahPlsMF7tvasucYINWJqHeAzyrLWnDdeJZvUCQ3A/vdM=
            -----END CERTIFICATE-----
            """.trimIndent()
    }
}
