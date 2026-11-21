package dev.beam.android.core.media.http

import okhttp3.Call
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrlOrNull
import okhttp3.Interceptor
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import uniffi.beam_client_core.ServerHttpConfig
import uniffi.beam_client_core.ServerSummary
import java.util.concurrent.ConcurrentHashMap

/**
 * Fetches as the signed-in user from a server's own origin, and as nobody from
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
 * Both are also scoped to the origin of the server they belong to, because the
 * cookie is a bearer credential: a request anywhere else goes out on the plain
 * client, and a redirect off the server's origin loses the credential at the
 * hop where it leaves (see [CredentialInterceptor]).
 *
 * The server is chosen by the request, not by which one is active: a download
 * queued from one server is still transferring when the viewer switches to
 * another, and its remaining requests must go out under its own server's
 * session (see [signedInServers]).
 *
 * @param server the config of the server a request's URL goes to, or `null`
 *   when no signed-in server serves it -- in which case the call goes out
 *   unauthenticated.
 */
public class ServerCallFactory internal constructor(
    private val clients: BeamHttpClientFactory,
    private val server: (url: HttpUrl) -> ServerHttpConfig?,
) : Call.Factory {
    /**
     * The client built for the last config seen from each origin. A config
     * changes only on a sign-in, sign-out or new trust decision, so rebuilding
     * per call would be pure waste; each wrapper shares the base client's
     * connection pool either way. One entry per server, so requests to two
     * servers in turn do not rebuild on every switch.
     */
    private val authenticated = ConcurrentHashMap<String, Pair<ServerHttpConfig, OkHttpClient>>()

    override fun newCall(request: Request): Call {
        val config = server(request.url)
        val origin = config?.let { it.baseUrl.toHttpUrlOrNull() }
        if (config == null || origin == null || !request.url.sameOrigin(origin)) {
            return clients.trusting(emptyList(), host = "").newCall(request)
        }
        return clientFor(config, origin).newCall(request)
    }

    private fun clientFor(
        config: ServerHttpConfig,
        origin: HttpUrl,
    ): OkHttpClient {
        authenticated[config.baseUrl]?.let { (cachedConfig, client) -> if (cachedConfig == config) return client }
        val client =
            clients
                // The pins hold only on connections to the host the core
                // recorded them for, so a redirect that leaves the server
                // meets the platform's verdict alone.
                .trusting(config.trustedFingerprints, config.host)
                .newBuilder()
                .addNetworkInterceptor(CredentialInterceptor(origin, config.headers))
                .build()
        authenticated[config.baseUrl] = config to client
        return client
    }

    internal companion object {
        /**
         * A factory that sends each request under the session of the known
         * server whose origin it goes to.
         *
         * The core keys a server by its origin, so at most one server matches.
         *
         * @param servers every known server, signed in or not.
         * @param configFor the config of one server by id, or `null` when it
         *   has no session.
         */
        fun signedInServers(
            clients: BeamHttpClientFactory,
            servers: () -> List<ServerSummary>,
            configFor: (serverId: String) -> ServerHttpConfig?,
        ): ServerCallFactory =
            ServerCallFactory(clients) { url ->
                servers()
                    .firstOrNull { server -> server.baseUrl.toHttpUrlOrNull()?.let(url::sameOrigin) == true }
                    ?.let { server -> configFor(server.id) }
            }
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

/**
 * Same scheme, host and port -- the web's definition of an origin, and so the
 * test for whether a request may carry the session.
 *
 * [HttpUrl] has already normalised what the web treats as equal: a host is
 * lower-cased, and a scheme's default port is filled in when none is written.
 */
internal fun HttpUrl.sameOrigin(other: HttpUrl): Boolean =
    scheme == other.scheme && host == other.host && port == other.port
