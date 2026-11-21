package dev.beam.android.feature.auth

import android.annotation.SuppressLint
import android.net.Uri
import android.net.http.SslCertificate
import android.net.http.SslError
import android.os.Build
import android.webkit.CookieManager
import android.webkit.SslErrorHandler
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView
import uniffi.beam_client_core.pinnedCertificateAccepted
import uniffi.beam_client_core.sessionCookieName

/** The cookie `beam-server` issues and reads, named by the core that sends it. */
internal val SESSION_COOKIE: String = sessionCookieName()

/**
 * The OIDC sign-in flow, hosted in an in-app browser.
 *
 * A [WebView] rather than a Custom Tab, and the reason is the cookie. The
 * server's only credential is an httpOnly `beam_session` cookie, and
 * `sanitize_redirect_path` accepts only same-origin relative paths, so the
 * provider cannot redirect to a custom scheme the app could intercept. A
 * Custom Tab's cookie jar is not readable by the app, so the credential would
 * be set somewhere the app can never see it. A WebView's jar is readable
 * through [CookieManager], which makes this the only flow the server supports
 * as it stands.
 *
 * This is an interim. A native token mint would be better on every axis, and
 * the [dev.beam.android.core.ffi] seam is shaped so it can replace this
 * without the screens changing.
 *
 * [trustedFingerprints] are the certificates the viewer accepted for the
 * server [url] is on; see [SignInWebViewClient] for what they admit.
 */
@SuppressLint("SetJavaScriptEnabled")
@Composable
internal fun SignInWebView(
    url: String,
    trustedFingerprints: List<String>,
    onSessionCookie: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    DisposableEffect(url) {
        onDispose {
            // Flushed on the way out so a cookie written just before the view
            // is torn down is not lost with it.
            CookieManager.getInstance().flush()
        }
    }

    AndroidView(
        modifier = modifier,
        factory = { context ->
            WebView(context).apply {
                // The provider's login page is an ordinary web page and will
                // not render without these; Dex's own form does not work with
                // JavaScript disabled.
                settings.javaScriptEnabled = true
                settings.domStorageEnabled = true
                CookieManager.getInstance().setAcceptThirdPartyCookies(this, true)

                webViewClient = SignInWebViewClient(url, trustedFingerprints, onSessionCookie)
                loadUrl(url)
            }
        },
        update = { view ->
            // Set again here, so a view that outlives a change of pins decides
            // by the current ones.
            view.webViewClient = SignInWebViewClient(url, trustedFingerprints, onSessionCookie)
            if (view.url != url && view.url == null) view.loadUrl(url)
        },
    )
}

/**
 * Whether a certificate the WebView refused may be accepted because the user
 * pinned it: the core's rule (`pinned_certificate_accepted`), the same one the
 * OkHttp stack's trust manager asks. A seam rather than a direct call so the
 * client can be tested on the JVM, where the core's native library is not
 * loaded.
 */
internal fun interface PinnedCertificateDecision {
    fun accepts(
        der: ByteArray,
        host: String,
        trustedFingerprints: List<String>,
        nowUnix: Long,
    ): Boolean
}

/** The core's rule, which is the only one production uses. */
internal object CorePinnedCertificateDecision : PinnedCertificateDecision {
    override fun accepts(
        der: ByteArray,
        host: String,
        trustedFingerprints: List<String>,
        nowUnix: Long,
    ): Boolean = pinnedCertificateAccepted(der, host, trustedFingerprints, nowUnix)
}

/**
 * The sign-in browser's client: watches for the session cookie, and lets a
 * certificate the user pinned through where the WebView would refuse it.
 *
 * Without the second, browser sign-in to a server with a self-signed
 * certificate fails even after the viewer accepted it, because the WebView
 * does not share the trust manager the app's own HTTP stack uses. The
 * exception is as narrow as that trust manager's: only on the host [signInUrl]
 * names -- the server the pins were recorded for, never the identity provider
 * a redirect leads to -- and only when the core's rule admits the certificate
 * for that host at this moment. Everything else is cancelled, which is what
 * the WebView does without an override.
 */
internal class SignInWebViewClient(
    private val signInUrl: String,
    private val trustedFingerprints: List<String>,
    private val onSessionCookie: (String) -> Unit,
    private val decision: PinnedCertificateDecision = CorePinnedCertificateDecision,
    private val nowUnix: () -> Long = { System.currentTimeMillis() / MILLIS_PER_SECOND },
) : WebViewClient() {
    override fun onPageFinished(
        view: WebView?,
        finishedUrl: String?,
    ) {
        super.onPageFinished(view, finishedUrl)
        // Checked on every completed navigation rather than on a single
        // expected redirect: the OIDC flow's exact hop count depends on the
        // provider and on whether the viewer already had a session, so the
        // cookie's appearance is the only reliable signal.
        sessionCookie(finishedUrl ?: signInUrl)?.let(onSessionCookie)
    }

    // Not calling super: its default is `cancel()`, which `decide` already
    // does for everything it does not admit.
    @SuppressLint("WebViewClientOnReceivedSslError")
    override fun onReceivedSslError(
        view: WebView?,
        handler: SslErrorHandler?,
        error: SslError?,
    ) {
        decide(error, proceed = { handler?.proceed() }, cancel = { handler?.cancel() })
    }

    /**
     * [proceed] when the core admits the presented certificate for this
     * server, otherwise [cancel]. Exactly one of them is called.
     *
     * Split from [onReceivedSslError] because an `SslErrorHandler` cannot be
     * constructed outside the framework.
     */
    internal fun decide(
        error: SslError?,
        proceed: () -> Unit,
        cancel: () -> Unit,
    ) {
        if (admits(error)) proceed() else cancel()
    }

    private fun admits(error: SslError?): Boolean {
        if (error == null || trustedFingerprints.isEmpty()) return false
        val pinnedHost = hostOf(signInUrl) ?: return false
        val host = error.url?.let { hostOf(it) } ?: return false
        if (host != pinnedHost) return false
        val der = error.certificate?.der() ?: return false
        return decision.accepts(der, host, trustedFingerprints, nowUnix())
    }

    private companion object {
        const val MILLIS_PER_SECOND = 1_000L

        /**
         * Where `SslCertificate` keeps the certificate in its saved state;
         * the only way to the encoded form before API 29.
         */
        const val SAVED_CERTIFICATE_KEY = "x509-certificate"

        /**
         * A URL's host as both sides spell it: lower-cased, without a trailing
         * root dot, and an IPv6 literal without its brackets.
         */
        fun hostOf(url: String): String? =
            Uri
                .parse(url)
                .host
                ?.removePrefix("[")
                ?.removeSuffix("]")
                ?.trimEnd('.')
                ?.lowercase()
                ?.takeIf(String::isNotEmpty)

        fun SslCertificate.der(): ByteArray? =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                x509Certificate?.encoded
            } else {
                SslCertificate.saveState(this).getByteArray(SAVED_CERTIFICATE_KEY)
            }
    }
}

/** The `beam_session` value from the cookie jar for [url], if it is there yet. */
internal fun sessionCookie(url: String): String? =
    CookieManager
        .getInstance()
        .getCookie(url)
        ?.split(';')
        ?.map(String::trim)
        ?.firstOrNull { it.startsWith("$SESSION_COOKIE=") }
        ?.substringAfter('=')
        ?.takeIf(String::isNotEmpty)
