package dev.beam.android.feature.auth

import android.annotation.SuppressLint
import android.webkit.CookieManager
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.ui.Modifier
import androidx.compose.ui.viewinterop.AndroidView

/** The cookie `beam-server` issues and reads. */
internal const val SESSION_COOKIE: String = "beam_session"

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
 */
@SuppressLint("SetJavaScriptEnabled")
@Composable
internal fun SignInWebView(
    url: String,
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

                webViewClient =
                    object : WebViewClient() {
                        override fun onPageFinished(
                            view: WebView?,
                            finishedUrl: String?,
                        ) {
                            super.onPageFinished(view, finishedUrl)
                            // Checked on every completed navigation rather than on
                            // a single expected redirect: the OIDC flow's exact
                            // hop count depends on the provider and on whether the
                            // viewer already had a session, so the cookie's
                            // appearance is the only reliable signal.
                            sessionCookie(finishedUrl ?: url)?.let(onSessionCookie)
                        }
                    }
                loadUrl(url)
            }
        },
        update = { view ->
            if (view.url != url && view.url == null) view.loadUrl(url)
        },
    )
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
