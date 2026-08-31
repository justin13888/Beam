package dev.beam.android.core.media.http

import java.security.MessageDigest
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509TrustManager

/**
 * Accepts the platform's chain, plus certificates the user explicitly trusted.
 *
 * This exists because OkHttp's `CertificatePinner` cannot do the job. Pinning
 * runs *after* the trust manager has already accepted a chain, so it can only
 * narrow trust -- it can never admit the self-signed certificate a LAN server
 * presents, which is the case Beam actually has to handle. Widening trust is a
 * trust manager's job, so that is what this is.
 *
 * The exception is deliberately narrow. A certificate is accepted only when its
 * whole-certificate SHA-256 matches one the user was shown and agreed to. There
 * is no "accept all", no host wildcard, and no relaxation of the public path:
 * the platform trust manager is always consulted first and its verdict is
 * final when it accepts.
 *
 * The digest matches [dev.beam.android.core.ffi] and the Rust core's
 * `tls.rs` -- whole DER certificate, not SubjectPublicKeyInfo -- so the app and
 * the core agree about which certificate the user approved, and it is the same
 * value `openssl x509 -fingerprint -sha256` prints.
 */
internal class TofuTrustManager(
    private val platform: X509TrustManager,
    trustedFingerprints: Collection<String>,
) : X509TrustManager {
    private val trusted: Set<String> = trustedFingerprints.map(::normalizeFingerprint).toSet()

    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
    ) {
        val certificates =
            chain?.takeIf { it.isNotEmpty() }
                // Checked here rather than left to the platform. A real platform
                // trust manager does reject an empty chain, but relying on that
                // makes this class's own safety depend on a delegate's unstated
                // behaviour -- and the failure mode would be accepting a peer that
                // presented no certificate at all.
                ?: throw CertificateException("no certificate presented")
        try {
            platform.checkServerTrusted(certificates, authType)
            return
        } catch (publicFailure: CertificateException) {
            val leaf = certificates.firstOrNull() ?: throw publicFailure
            if (fingerprintOf(leaf) !in trusted) {
                // Rethrowing the platform's own failure keeps the diagnostic
                // the platform produced, which is more informative than
                // anything this class could invent.
                throw publicFailure
            }
            // Hostname is still verified, by OkHttp's verifier over the SANs of
            // this same certificate. A pin is permission to trust one
            // certificate for one server, never a blanket exemption.
        }
    }

    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
    ) {
        platform.checkClientTrusted(chain, authType)
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = platform.acceptedIssuers

    internal companion object {
        /** The platform's own trust manager, over the system CA store. */
        fun platformTrustManager(): X509TrustManager {
            val factory = TrustManagerFactory.getInstance(TrustManagerFactory.getDefaultAlgorithm())
            factory.init(null as java.security.KeyStore?)
            return factory.trustManagers
                .filterIsInstance<X509TrustManager>()
                .firstOrNull()
                ?: error("the platform provides no X509TrustManager")
        }

        /** An SSL socket factory backed by [manager]. */
        fun socketFactory(manager: X509TrustManager): javax.net.ssl.SSLSocketFactory {
            val context = SSLContext.getInstance("TLS")
            context.init(null, arrayOf(manager), java.security.SecureRandom())
            return context.socketFactory
        }
    }
}

/**
 * Compare digests without being defeated by formatting.
 *
 * The same digest arrives as `AB:CD:EF`, `ab:cd:ef` or `abcdef` depending on
 * whether it came from the core, from storage, or from a paste. Comparing raw
 * strings would silently reject a certificate the user had already trusted --
 * and the symptom would be an unexplained connection failure, not an obvious
 * bug.
 */
internal fun normalizeFingerprint(value: String): String = value.filter { it.isDigit() || it in 'a'..'f' || it in 'A'..'F' }.lowercase()

/** Whole-certificate SHA-256, lowercase hex with no separators. */
internal fun fingerprintOf(certificate: X509Certificate): String =
    MessageDigest
        .getInstance("SHA-256")
        .digest(certificate.encoded)
        .joinToString(separator = "") { byte -> "%02x".format(byte) }
