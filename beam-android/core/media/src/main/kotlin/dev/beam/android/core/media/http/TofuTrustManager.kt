package dev.beam.android.core.media.http

import android.annotation.SuppressLint
import uniffi.beam_client_core.pinnedCertificateAccepted
import java.net.Socket
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLEngine
import javax.net.ssl.SSLSocket
import javax.net.ssl.TrustManagerFactory
import javax.net.ssl.X509ExtendedTrustManager
import javax.net.ssl.X509TrustManager

/**
 * Whether a certificate the platform refused may be accepted because the user
 * pinned it.
 *
 * The rule is the core's (`pinned_certificate_accepted`, beside the verifier
 * the core's own client uses): the whole-certificate SHA-256 is one the user
 * accepted, the certificate covers the host, and the time is inside its
 * validity window. Android decides by calling it rather than keeping a second
 * copy, because a second copy has already drifted once -- it checked the
 * fingerprint alone, and accepted an expired pinned certificate the core
 * refused. A seam rather than a direct call so the trust manager can be tested
 * on the JVM, where the core's native library is not loaded.
 */
internal fun interface PinnedCertificatePolicy {
    fun accepts(
        der: ByteArray,
        host: String,
        trustedFingerprints: List<String>,
        nowUnix: Long,
    ): Boolean
}

/** The core's rule, which is the only one production uses. */
internal object CorePinnedCertificatePolicy : PinnedCertificatePolicy {
    override fun accepts(
        der: ByteArray,
        host: String,
        trustedFingerprints: List<String>,
        nowUnix: Long,
    ): Boolean = pinnedCertificateAccepted(der, host, trustedFingerprints, nowUnix)
}

/**
 * Accepts the platform's chain, plus certificates the user explicitly trusted
 * for [pinnedHost].
 *
 * This exists because OkHttp's `CertificatePinner` cannot do the job. Pinning
 * runs *after* the trust manager has already accepted a chain, so it can only
 * narrow trust -- it can never admit the self-signed certificate a LAN server
 * presents, which is the case Beam actually has to handle. Widening trust is a
 * trust manager's job, so that is what this is.
 *
 * The exception is deliberately narrow. The platform trust manager is always
 * consulted first and its verdict is final when it accepts. Only when it
 * refuses, and only on a connection to [pinnedHost] -- the server the pins were
 * recorded for, never a host a redirect leads to -- is [policy] asked, and it
 * decides by the core's rule. There is no "accept all" and no host wildcard.
 *
 * Extended rather than plain [X509TrustManager] because only the extended
 * overloads say which host the connection is to; both Conscrypt and the JDK
 * call them when the trust manager offers them.
 */
@SuppressLint("CustomX509TrustManager")
internal class TofuTrustManager(
    private val platform: X509TrustManager,
    private val trustedFingerprints: List<String>,
    pinnedHost: String,
    private val policy: PinnedCertificatePolicy = CorePinnedCertificatePolicy,
    private val nowUnix: () -> Long = { System.currentTimeMillis() / MILLIS_PER_SECOND },
) : X509ExtendedTrustManager() {
    private val pinnedHost: String = canonicalHost(pinnedHost)

    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        socket: Socket?,
    ) {
        check(chain, authType, (socket as? SSLSocket)?.handshakeSession?.peerHost)
    }

    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        engine: SSLEngine?,
    ) {
        check(chain, authType, engine?.handshakeSession?.peerHost ?: engine?.peerHost)
    }

    /**
     * Without a connection to read the host from, the pin is held to the host
     * it was recorded for. Only a caller outside any TLS stack reaches this:
     * both stacks Beam runs on call the overloads above.
     */
    override fun checkServerTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
    ) {
        check(chain, authType, pinnedHost)
    }

    private fun check(
        chain: Array<out X509Certificate>?,
        authType: String?,
        peerHost: String?,
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
        } catch (publicFailure: CertificateException) {
            // Rethrowing the platform's own failure keeps the diagnostic the
            // platform produced, which is more informative than anything this
            // class could invent.
            val host = peerHost?.let(::canonicalHost)
            if (host == null || pinnedHost.isEmpty() || host != pinnedHost) throw publicFailure
            val leaf = certificates.first()
            if (!policy.accepts(leaf.encoded, host, trustedFingerprints, nowUnix())) throw publicFailure
            // Hostname is still verified, by OkHttp's verifier over the SANs of
            // this same certificate. A pin is permission to trust one
            // certificate for one server, never a blanket exemption.
        }
    }

    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        socket: Socket?,
    ) {
        platform.checkClientTrusted(chain, authType)
    }

    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
        engine: SSLEngine?,
    ) {
        platform.checkClientTrusted(chain, authType)
    }

    override fun checkClientTrusted(
        chain: Array<out X509Certificate>?,
        authType: String?,
    ) {
        platform.checkClientTrusted(chain, authType)
    }

    override fun getAcceptedIssuers(): Array<X509Certificate> = platform.acceptedIssuers

    internal companion object {
        private const val MILLIS_PER_SECOND = 1_000L

        /**
         * A host as both sides spell it: lower-cased, without a trailing root
         * dot, and an IPv6 literal without the brackets a URL wraps it in --
         * the core reports `[::1]`, the TLS stack `::1`.
         */
        fun canonicalHost(host: String): String =
            host
                .trim()
                .removePrefix("[")
                .removeSuffix("]")
                .trimEnd('.')
                .lowercase()

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
