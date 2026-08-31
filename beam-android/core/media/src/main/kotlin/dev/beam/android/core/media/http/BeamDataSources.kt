package dev.beam.android.core.media.http

import android.content.Context
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DefaultDataSource
import androidx.media3.datasource.okhttp.OkHttpDataSource
import okhttp3.OkHttpClient
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
) {
    fun trusting(fingerprints: List<String>): OkHttpClient {
        if (fingerprints.isEmpty()) return shared

        val platform = TofuTrustManager.platformTrustManager()
        val manager = TofuTrustManager(platform, fingerprints)
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
 * A Media3 [DataSource.Factory] that authenticates as the signed-in user.
 *
 * Wrapped in [DefaultDataSource.Factory] so `file://` and `content://` URIs
 * resolve too -- without it, a completed download would be unplayable, because
 * an HTTP-only factory cannot open a local file.
 *
 * Takes the credential and trust decision rather than a whole config, because
 * downloads and playback need the same factory built from different shapes:
 * a download manager is constructed once, before any file is chosen.
 */
internal fun beamDataSourceFactory(
    context: Context,
    clients: BeamHttpClientFactory,
    headers: Map<String, String>,
    trustedFingerprints: List<String>,
): DataSource.Factory {
    val http =
        OkHttpDataSource
            .Factory(clients.trusting(trustedFingerprints))
            .setDefaultRequestProperties(headers)
    return DefaultDataSource.Factory(context, http)
}
