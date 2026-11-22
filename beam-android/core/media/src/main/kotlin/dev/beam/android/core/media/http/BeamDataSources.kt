// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.http

import android.content.Context
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DefaultDataSource
import androidx.media3.datasource.HttpDataSource
import androidx.media3.datasource.cache.Cache
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.okhttp.OkHttpDataSource
import okhttp3.Call
import okhttp3.OkHttpClient
import uniffi.beam_client_core.ServerHttpConfig
import java.util.concurrent.TimeUnit

/**
 * Builds the HTTP stack Media3 uses to fetch media bytes.
 *
 * Playback must agree with the API client about two things or it fails in ways
 * that look like corrupt media rather than an auth or trust problem: who the
 * user is (the `beam_session` cookie) and which certificate is acceptable. The
 * core resolves both and hands them over as a [PlaybackHttpConfig]; this turns
 * that into an OkHttp client.
 *
 * A per-request factory rather than one shared client, because the trust set
 * and the cookie are per-server and a viewer can switch servers mid-session.
 * The underlying connection pool and dispatcher are shared, so switching costs
 * a wrapper, not a new pool -- which matters because ExoPlayer issues many
 * neighbouring range requests and would otherwise re-handshake for each.
 */
internal class BeamHttpClientFactory(
    private val shared: OkHttpClient,
    private val pins: PinnedCertificatePolicy = CorePinnedCertificatePolicy,
) {
    /**
     * A client that also accepts the certificates the user pinned for [host],
     * on connections to [host] alone.
     */
    fun trusting(
        fingerprints: List<String>,
        host: String,
    ): OkHttpClient {
        if (fingerprints.isEmpty()) return shared

        val platform = TofuTrustManager.platformTrustManager()
        val manager = TofuTrustManager(platform, fingerprints, host, pins)
        return shared
            .newBuilder()
            .sslSocketFactory(TofuTrustManager.socketFactory(manager), manager)
            .build()
    }

    internal companion object {
        /**
         * Timeouts sized for streaming, not for API calls.
         *
         * Read timeout is generous because a range request against a large
         * file on a slow disk can legitimately stall well past the default,
         * and a spurious timeout mid-playback surfaces to the viewer as a
         * stutter or a stop. Call timeout stays unset for the same reason: a
         * long-lived streaming response is not a hung request.
         */
        fun shared(): OkHttpClient =
            OkHttpClient
                .Builder()
                .connectTimeout(15, TimeUnit.SECONDS)
                .readTimeout(60, TimeUnit.SECONDS)
                .retryOnConnectionFailure(true)
                .build()
    }
}

/**
 * The player's [DataSource.Factory]: the download [cache] first, then the
 * network as the signed-in user.
 *
 * Downloads land in [cache] (see `BeamDownloadManager`), keyed by the URL
 * they were fetched from. Reading through a [CacheDataSource] is what lets a
 * downloaded file play with no network at all: every span on disk is read from
 * disk, and only a span that is not goes upstream.
 *
 * Read-only. No cache sink is set, so streaming never writes to [cache]: it
 * holds what the viewer chose to download and nothing else, under a
 * no-eviction policy that would otherwise keep every streamed byte forever. A
 * span a download is writing at that moment is locked, and is read from the
 * network rather than waited for; a cache that fails to read is skipped the
 * same way, since the network can still serve the file.
 *
 * Upstream is wrapped in [DefaultDataSource.Factory] so `file://` and
 * `content://` URIs resolve too.
 *
 * Takes the server's config rather than one file's: playback resolves it once
 * for the file it is about to play.
 */
internal fun beamDataSourceFactory(
    context: Context,
    clients: BeamHttpClientFactory,
    server: ServerHttpConfig,
    cache: Cache,
): DataSource.Factory =
    CacheDataSource
        .Factory()
        .setCache(cache)
        .setUpstreamDataSourceFactory(DefaultDataSource.Factory(context, beamHttpDataSourceFactory(clients, server)))
        .setCacheWriteDataSinkFactory(null)
        .setFlags(CacheDataSource.FLAG_IGNORE_CACHE_ON_ERROR)

/**
 * A Media3 [DataSource.Factory] whose requests go out through [calls].
 *
 * For a consumer that outlives any one credential -- the download manager is
 * built once per process, and a download runs across sign-outs, re-sign-ins and
 * restarts -- so [calls] must resolve the server config per request, as
 * [ServerCallFactory] does, rather than capture it.
 */
internal fun beamDataSourceFactory(
    context: Context,
    calls: Call.Factory,
): DataSource.Factory = DefaultDataSource.Factory(context, OkHttpDataSource.Factory(calls))

/**
 * The HTTP half of [beamDataSourceFactory]: requests go out through a
 * [ServerCallFactory], so the session and the pins apply on [server]'s origin
 * and nowhere else.
 *
 * Not `setDefaultRequestProperties`: those headers go on every request the
 * data source makes, to whatever host a media URL or a redirect names. The
 * cookie is a bearer credential, so it is attached per hop, and only on hops
 * to the server's own origin.
 */
internal fun beamHttpDataSourceFactory(
    clients: BeamHttpClientFactory,
    server: ServerHttpConfig,
): HttpDataSource.Factory = OkHttpDataSource.Factory(ServerCallFactory(clients) { server })
