// Media3's DataSource surface is @UnstableApi; see BeamDataSources.kt.
@file:UnstableApi

package dev.beam.android.core.media.http

import android.net.Uri
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DataSpec
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.HttpUrl
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.beam_client_core.ServerHttpConfig
import java.net.InetAddress
import java.util.concurrent.TimeUnit

/**
 * The data source Media3 streams and downloads through, against real servers
 * on loopback. Media bytes are behind the session like everything else on the
 * server, so the cookie has to reach the server -- and nowhere else, including
 * wherever a redirect leads.
 */
@RunWith(RobolectricTestRunner::class)
class BeamDataSourcesTest {
    /** The signed-in server: bytes for the session holder, 401 for anyone else. */
    private val beam = MockWebServer()

    /** Some other host, which serves anyone and records what it was sent. */
    private val elsewhere = MockWebServer()

    @Before
    fun setUp() {
        beam.dispatcher = BeamServer()
        beam.start(InetAddress.getByName(SERVER_HOST), 0)
        elsewhere.dispatcher = OpenServer()
        elsewhere.start(InetAddress.getByName(OTHER_HOST), 0)
    }

    @After
    fun tearDown() {
        beam.close()
        elsewhere.close()
    }

    @Test
    fun `media requests carry the session only to the server origin`() {
        read(beam.url("/v1/files/f1/stream"))
        read(elsewhereUrl())

        assertEquals(SESSION, beam.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)?.headers?.get("Cookie"))
        val other = elsewhere.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)
        assertNotNull("the other host was reached", other)
        assertNull("the session never goes to another origin", other!!.headers["Cookie"])
    }

    @Test
    fun `a redirect off origin drops the session`() {
        read(beam.url("/v1/redirect"))

        assertEquals(SESSION, beam.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)?.headers?.get("Cookie"))
        val followed = elsewhere.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)
        assertNotNull("the redirect was followed", followed)
        assertNull("the hop that leaves the server loses the session", followed!!.headers["Cookie"])
    }

    /** Open, drain and close one data source for [url], as a player would. */
    private fun read(url: HttpUrl) {
        val origin = beam.url("/")
        val source =
            beamHttpDataSourceFactory(
                BeamHttpClientFactory(BeamHttpClientFactory.shared()),
                ServerHttpConfig(
                    baseUrl = origin.toString().trimEnd('/'),
                    headers = mapOf("Cookie" to SESSION),
                    trustedFingerprints = emptyList(),
                    host = origin.host,
                ),
            ).createDataSource()
        try {
            source.open(DataSpec(Uri.parse(url.toString())))
            val buffer = ByteArray(BODY.length)
            while (source.read(buffer, 0, buffer.size) != androidx.media3.common.C.RESULT_END_OF_INPUT) {
                // Drained; the bytes themselves are not the point.
            }
        } finally {
            source.close()
        }
    }

    /**
     * Built by hand because the server's own URL names it by reverse lookup,
     * which on loopback is the signed-in server's name too.
     */
    private fun elsewhereUrl(): HttpUrl =
        HttpUrl
            .Builder()
            .scheme("http")
            .host(OTHER_HOST)
            .port(elsewhere.port)
            .encodedPath("/media.mkv")
            .build()

    private inner class BeamServer : Dispatcher() {
        override fun dispatch(request: RecordedRequest): MockResponse =
            when {
                request.headers["Cookie"] != SESSION -> {
                    MockResponse.Builder().code(401).build()
                }

                request.url.encodedPath == "/v1/redirect" -> {
                    MockResponse
                        .Builder()
                        .code(302)
                        .setHeader("Location", elsewhereUrl().toString())
                        .build()
                }

                else -> {
                    bytes()
                }
            }
    }

    private class OpenServer : Dispatcher() {
        override fun dispatch(request: RecordedRequest): MockResponse = bytes()
    }

    private companion object {
        const val SESSION = "beam_session=s3cret"
        const val TIMEOUT_SECONDS = 5L
        const val SERVER_HOST = "localhost"

        /** Another host than the signed-in server's, so the two differ in host, not only in port. */
        const val OTHER_HOST = "127.0.0.1"

        const val BODY = "media bytes"

        fun bytes(): MockResponse =
            MockResponse
                .Builder()
                .code(200)
                .setHeader("Content-Type", "video/x-matroska")
                .body(BODY)
                .build()
    }
}
