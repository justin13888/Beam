package dev.beam.android.core.media.http

import okhttp3.Call
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import uniffi.beam_client_core.ServerHttpConfig

/**
 * Fetches as the signed-in user from the active server, and as nobody from
 * anywhere else.
 *
 * Artwork is served from the same authenticated origin as the API
 * (`GET /v1/artwork/...` requires the session), so an image loader on a bare
 * client gets a 401 for every poster. The credential and the trust decision
 * live in the core; this applies them the way Media3's data sources do --
 * [ServerHttpConfig.headers] and [BeamHttpClientFactory.trusting] -- for any
 * caller that just wants a [Call.Factory].
 *
 * Both are resolved per call rather than captured once, because the viewer
 * can sign out or switch servers while the app is running, and a loader
 * built at app start would otherwise keep sending the first server's cookie.
 *
 * Both are also scoped to the active server's origin, because the cookie is a
 * bearer credential: a request anywhere else goes out on the plain client,
 * and a redirect off the server's origin loses the credential at the hop
 * where it leaves (see [CredentialInterceptor]).
 *
 * @param server the active server's config, or `null` when there is no signed-in
 *   server -- in which case every call goes out unauthenticated.
 */
public class ServerCallFactory internal constructor(
    private val clients: BeamHttpClientFactory,
    private val server: () -> ServerHttpConfig?,
) : Call.Factory {
    /**
     * The client built for the last config seen. A config changes only on a
     * sign-in, sign-out, server switch or new trust decision, so rebuilding
     * per call would be pure waste; the wrapper shares the base client's
     * connection pool either way.
     */
    @Volatile
    private var authenticated: Pair<ServerHttpConfig, OkHttpClient>? = null

    override fun newCall(request: Request): Call {
        val config = server()
        val origin = config?.let { it.baseUrl.toHttpUrlOrNull() }
        if (config == null || origin == null || !request.url.sameOrigin(origin)) {
            return clients.trusting(emptyList()).newCall(request)
        }
        return clientFor(config, origin).newCall(request)
    }

    private fun clientFor(
        config: ServerHttpConfig,
        origin: HttpUrl,
    ): OkHttpClient {
        authenticated?.let { (cachedConfig, client) -> if (cachedConfig == config) return client }
        val client =
            clients
                .trusting(config.trustedFingerprints)
                .newBuilder()
                .addNetworkInterceptor(CredentialInterceptor(origin, config.headers))
                .build()
        authenticated = config to client
        return client
    }
}

/**
 * Attaches [headers] to each request that goes to [origin], and to nothing
 * else.
 *
 * A network interceptor, not an application one, because OkHttp follows
 * redirects between the two: an application interceptor sees only the first
 * request, and a `Cookie` header set there is carried to wherever the server
 * redirects (OkHttp strips `Authorization` on a cross-host redirect, but not a
 * cookie a caller set by hand). Here every hop is checked on its own.
 */
private class CredentialInterceptor(
    private val origin: HttpUrl,
    private val headers: Map<String, String>,
) : Interceptor {
    override fun intercept(chain: Interceptor.Chain): Response {
        val request = chain.request()
        if (!request.url.sameOrigin(origin)) return chain.proceed(request)
        val authenticated =
            request
                .newBuilder()
                .apply { headers.forEach { (name, value) -> header(name, value) } }
                .build()
        return chain.proceed(authenticated)
    }
}

/** Same scheme, host and port -- the web's definition of an origin. */
private fun HttpUrl.sameOrigin(other: HttpUrl): Boolean =
    scheme == other.scheme && host == other.host && port == other.port
