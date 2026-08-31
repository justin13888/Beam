import SwiftUI
import WebKit

/// The in-app browser the sign-in flow runs in, and the cookie lift.
///
/// A `WKWebView` rather than `ASWebAuthenticationSession`, which would be the
/// right tool: its cookie jar is deliberately not readable by the app, and the
/// `beam_session` cookie is the only credential `beam-server` accepts. See
/// NFR-605 -- this is a recorded limitation of the server's auth model, not a
/// preference, and the interface exists so a native token mint can replace it
/// without this screen's callers changing.
///
/// The cookie is read from `WKHTTPCookieStore` after every navigation rather
/// than by matching a redirect URL, because the server chooses where to send
/// the browser after the exchange and matching that would couple this screen
/// to a redirect path the server is free to change.
struct SignInWebView {
    /// Where to start.
    let url: URL
    /// The origin whose cookie is wanted, so another host's cookie is never
    /// lifted from a redirect through an identity provider.
    let host: String
    /// Called once, with the cookie's `name=value` pair.
    let onCookie: (String) -> Void

    /// The cookie `beam-server` sets and reads.
    static let cookieName = "beam_session"
}

#if os(iOS)
extension SignInWebView: UIViewRepresentable {
    func makeUIView(context: Context) -> WKWebView {
        context.coordinator.makeWebView(url: url)
    }

    func updateUIView(_ webView: WKWebView, context: Context) {}

    func makeCoordinator() -> SignInCoordinator {
        SignInCoordinator(host: host, onCookie: onCookie)
    }
}
#else
extension SignInWebView: NSViewRepresentable {
    func makeNSView(context: Context) -> WKWebView {
        context.coordinator.makeWebView(url: url)
    }

    func updateNSView(_ webView: WKWebView, context: Context) {}

    func makeCoordinator() -> SignInCoordinator {
        SignInCoordinator(host: host, onCookie: onCookie)
    }
}
#endif

/// Drives the web view and watches its cookie store.
final class SignInCoordinator: NSObject, WKNavigationDelegate {
    private let host: String
    private let onCookie: (String) -> Void
    private var delivered = false

    init(host: String, onCookie: @escaping (String) -> Void) {
        self.host = host
        self.onCookie = onCookie
    }

    func makeWebView(url: URL) -> WKWebView {
        let configuration = WKWebViewConfiguration()
        // A non-persistent store, so the sign-in leaves nothing behind on the
        // device and a second sign-in starts clean rather than silently
        // reusing a session the user thought they had ended.
        configuration.websiteDataStore = .nonPersistent()

        let webView = WKWebView(frame: .zero, configuration: configuration)
        webView.navigationDelegate = self
        webView.load(URLRequest(url: url))
        return webView
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        checkForCookie(in: webView)
    }

    func webView(
        _ webView: WKWebView,
        didFailProvisionalNavigation navigation: WKNavigation!,
        withError error: Error
    ) {
        // A provisional failure is routine here -- the identity provider
        // redirects through hosts that may refuse the app's user agent -- and
        // the cookie may already have been set before it happened.
        checkForCookie(in: webView)
    }

    private func checkForCookie(in webView: WKWebView) {
        guard !delivered else { return }
        webView.configuration.websiteDataStore.httpCookieStore.getAllCookies {
            [weak self] cookies in
            guard let self, !self.delivered else { return }
            guard
                let cookie = cookies.first(where: {
                    $0.name == SignInWebView.cookieName && self.matchesHost($0.domain)
                })
            else {
                return
            }
            self.delivered = true
            self.onCookie("\(cookie.name)=\(cookie.value)")
        }
    }

    /// Whether a cookie's domain covers the server we are signing in to.
    ///
    /// Without this, a cookie called `beam_session` set by some other host in
    /// the redirect chain would be lifted and handed to the core as if it were
    /// the server's.
    func matchesHost(_ domain: String) -> Bool {
        let normalized = domain.hasPrefix(".") ? String(domain.dropFirst()) : domain
        return host == normalized || host.hasSuffix(".\(normalized)")
    }
}
