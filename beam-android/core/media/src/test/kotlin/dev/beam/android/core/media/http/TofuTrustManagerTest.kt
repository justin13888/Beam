package dev.beam.android.core.media.http

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.X509TrustManager

class TofuTrustManagerTest {
    @Test
    fun `a fingerprint is matched regardless of formatting`() {
        // The same digest reaches the app colon-grouped from the core, bare
        // from storage, and space-separated from a paste. Comparing raw
        // strings would reject a certificate the user had already accepted,
        // and the symptom would look like a network fault.
        assertEquals("abcdef", normalizeFingerprint("AB:CD:EF"))
        assertEquals("abcdef", normalizeFingerprint("ab cd ef"))
        assertEquals("abcdef", normalizeFingerprint("abcdef"))
    }

    @Test
    fun `separators are discarded rather than compared`() {
        assertEquals("aabb", normalizeFingerprint("aa-bb"))
        assertEquals("aabb", normalizeFingerprint("aa:bb"))
    }

    @Test
    fun `a certificate the platform accepts is accepted without consulting pins`() {
        val platform = FakeTrustManager(accepts = true)
        val manager = TofuTrustManager(platform, emptyList())

        manager.checkServerTrusted(arrayOf(certificate()), "RSA")

        assertTrue("the platform path must be tried first", platform.wasConsulted)
    }

    @Test
    fun `a certificate the platform rejects is accepted when the user pinned it`() {
        // The case the whole class exists for: a self-signed certificate on a
        // LAN server that the user was shown and agreed to.
        val manager =
            TofuTrustManager(
                FakeTrustManager(accepts = false),
                listOf(PINNED_FINGERPRINT),
            )

        manager.checkServerTrusted(arrayOf(certificate()), "RSA")
    }

    @Test
    fun `a pin recorded in another format still matches`() {
        val bare = PINNED_FINGERPRINT.replace(":", "").lowercase()
        val manager = TofuTrustManager(FakeTrustManager(accepts = false), listOf(bare))

        manager.checkServerTrusted(arrayOf(certificate()), "RSA")
    }

    @Test
    fun `a different certificate is rejected even when one is pinned`() {
        // The substitution a pin exists to detect. Flipping one hex digit
        // stands in for a different certificate presented for the same host.
        val wrong = PINNED_FINGERPRINT.replaceFirst("F8", "F9")
        val manager = TofuTrustManager(FakeTrustManager(accepts = false), listOf(wrong))

        val failure = runCatching { manager.checkServerTrusted(arrayOf(certificate()), "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
    }

    @Test
    fun `the computed fingerprint matches what openssl prints`() {
        // Load-bearing: the user is asked to compare the app's digest against
        // their server's. If this drifts, they would be comparing two
        // different things and the comparison would be worthless.
        assertEquals(
            PINNED_FINGERPRINT.replace(":", "").lowercase(),
            fingerprintOf(certificate()),
        )
    }

    @Test
    fun `a certificate the platform rejects and the user has not pinned is rejected`() {
        val manager = TofuTrustManager(FakeTrustManager(accepts = false), emptyList())

        val failure = runCatching { manager.checkServerTrusted(arrayOf(certificate()), "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
    }

    @Test
    fun `an empty chain is rejected rather than treated as trusted`() {
        val manager = TofuTrustManager(FakeTrustManager(accepts = true), emptyList())

        val failure = runCatching { manager.checkServerTrusted(emptyArray(), "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
    }

    @Test
    fun `a null chain is rejected`() {
        val manager = TofuTrustManager(FakeTrustManager(accepts = true), emptyList())

        val failure = runCatching { manager.checkServerTrusted(null, "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
    }

    @Test
    fun `client trust is delegated untouched`() {
        // The exception this class makes is for *servers*. Widening client
        // trust would be a different decision entirely, and nobody asked for
        // it.
        val platform = FakeTrustManager(accepts = false)
        val manager = TofuTrustManager(platform, listOf("aa"))

        val failure = runCatching { manager.checkClientTrusted(arrayOf(certificate()), "RSA") }

        assertTrue(failure.exceptionOrNull() is CertificateException)
        assertFalse(
            "a server pin must never admit a client certificate",
            failure.isSuccess,
        )
    }

    private fun certificate(): X509Certificate =
        java.security.cert.CertificateFactory
            .getInstance("X.509")
            .generateCertificate(SELF_SIGNED_PEM.byteInputStream()) as X509Certificate

    private class FakeTrustManager(
        private val accepts: Boolean,
    ) : X509TrustManager {
        var wasConsulted: Boolean = false
            private set

        override fun checkServerTrusted(
            chain: Array<out X509Certificate>?,
            authType: String?,
        ) {
            wasConsulted = true
            if (!accepts) throw CertificateException("platform rejected the chain")
        }

        override fun checkClientTrusted(
            chain: Array<out X509Certificate>?,
            authType: String?,
        ) {
            if (!accepts) throw CertificateException("platform rejected the chain")
        }

        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    private companion object {
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
