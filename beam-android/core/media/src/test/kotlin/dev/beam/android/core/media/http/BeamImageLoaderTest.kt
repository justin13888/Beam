package dev.beam.android.core.media.http

import android.content.Context
import android.util.Base64
import androidx.test.core.app.ApplicationProvider
import coil3.ImageLoader
import coil3.request.CachePolicy
import coil3.request.ErrorResult
import coil3.request.ImageRequest
import coil3.request.ImageResult
import coil3.request.SuccessResult
import dev.beam.android.core.media.download.DownloadArtwork
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okhttp3.HttpUrl
import okhttp3.tls.HandshakeCertificates
import okhttp3.tls.HeldCertificate
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode
import uniffi.beam_client_core.ServerHttpConfig
import java.net.InetAddress
import java.util.concurrent.TimeUnit

/**
 * The app's image loader, as the application builds it, against real servers
 * on loopback.
 *
 * Artwork is behind the session (`GET /v1/artwork/...` answers 401 without
 * it), so the servers here do what Beam does: refuse a request that does not
 * carry the cookie. Only the core is stood in for -- by the [ServerHttpConfig]
 * it would hand over -- because the credential is the core's to resolve and
 * this is about what the loader does with it.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class BeamImageLoaderTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val context: Context = ApplicationProvider.getApplicationContext()

    /** The signed-in server: artwork for the session holder, 401 for anyone else. */
    private val beam = MockWebServer()

    /** The signed-in server again, behind a certificate no CA vouches for. */
    private val secure = MockWebServer()

    /** Some other host, which serves anyone and records what it was sent. */
    private val elsewhere = MockWebServer()

    /** What the core reports: the active server and its session, or none. */
    private var active: ServerHttpConfig? = null

    private lateinit var loader: ImageLoader

    @Before
    fun setUp() {
        // Coil hops to the main dispatcher for its interceptor chain, and under
        // Robolectric that is the paused main looper this test runs on.
        Dispatchers.setMain(UnconfinedTestDispatcher())
        beam.dispatcher = BeamServer()
        beam.start(InetAddress.getByName(SERVER_HOST), 0)
        elsewhere.dispatcher = OpenServer()
        elsewhere.start(InetAddress.getByName(OTHER_HOST), 0)
        loader =
            BeamImageLoader.build(
                context = context,
                calls = ServerCallFactory(BeamHttpClientFactory(BeamHttpClientFactory.shared())) { active },
                diskCacheDirectory = temp.newFolder("artwork"),
            )
    }

    @After
    fun tearDown() {
        loader.shutdown()
        beam.close()
        secure.close()
        elsewhere.close()
        Dispatchers.resetMain()
    }

    @Test
    fun `artwork from the signed-in server renders`() =
        runTest {
            signIn()

            val result = render(beam.url("/v1/artwork/media/m1/poster"))

            assertTrue("rendered, got $result", result is SuccessResult)
            assertEquals(SESSION, beam.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)?.headers?.get("Cookie"))
        }

    @Test
    fun `a download's poster is pinned from the signed-in server`() =
        runTest {
            signIn()
            val poster = beam.url("/v1/artwork/media/m1/poster").toString()

            DownloadArtwork(context) { loader }.pin(poster)

            assertTrue(
                "the poster is on disk for the downloads screen",
                loader.diskCache!!.openSnapshot(poster)?.use { true } ?: false,
            )
        }

    @Test
    fun `the session is never sent to another host`() =
        runTest {
            signIn()

            val result = render(elsewhereUrl())

            assertTrue("rendered, got $result", result is SuccessResult)
            val request = elsewhere.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)
            assertNotNull("the other host was reached", request)
            assertNull(request!!.headers["Cookie"])
        }

    @Test
    fun `a redirect off the server drops the session at the hop that leaves`() =
        runTest {
            signIn()

            val result = render(beam.url("/v1/redirect"))

            assertTrue("rendered, got $result", result is SuccessResult)
            assertEquals(SESSION, beam.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)?.headers?.get("Cookie"))
            val followed = elsewhere.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)
            assertNotNull("the redirect was followed", followed)
            assertNull(followed!!.headers["Cookie"])
        }

    @Test
    fun `signing out stops sending the session on the next fetch`() =
        runTest {
            signIn()
            assertTrue(render(beam.url("/v1/artwork/media/m1/poster")) is SuccessResult)
            beam.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)

            active = null
            val result = render(beam.url("/v1/artwork/media/m2/poster"))

            assertTrue("refused once signed out, got $result", result is ErrorResult)
            assertNull(beam.takeRequest(TIMEOUT_SECONDS, TimeUnit.SECONDS)?.headers?.get("Cookie"))
        }

    @Test
    fun `artwork from a self-signed server the user trusted renders`() =
        runTest {
            // The LAN server the trust prompt exists for: its certificate is in
            // no CA store, and the user accepted its fingerprint.
            val certificate =
                HeldCertificate
                    .Builder()
                    .addSubjectAlternativeName(SERVER_HOST)
                    .build()
            secure.useHttps(
                HandshakeCertificates
                    .Builder()
                    .heldCertificate(certificate)
                    .build()
                    .sslSocketFactory(),
            )
            secure.dispatcher = BeamServer()
            secure.start(InetAddress.getByName(SERVER_HOST), 0)
            signIn(server = secure, trustedFingerprints = listOf(fingerprintOf(certificate.certificate)))

            val result = render(secure.url("/v1/artwork/media/m1/poster"))

            assertTrue("rendered, got $result", result is SuccessResult)
        }

    private fun signIn(
        server: MockWebServer = beam,
        trustedFingerprints: List<String> = emptyList(),
    ) {
        val origin = server.url("/")
        active =
            ServerHttpConfig(
                baseUrl = origin.toString().trimEnd('/'),
                headers = mapOf("Cookie" to SESSION),
                trustedFingerprints = trustedFingerprints,
                host = origin.host,
            )
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
            .encodedPath("/poster.png")
            .build()

    /** What `Artwork` asks for, minus the memory cache a fresh screen would not have. */
    private suspend fun render(url: HttpUrl): ImageResult =
        loader.execute(
            ImageRequest
                .Builder(context)
                .data(url.toString())
                .memoryCachePolicy(CachePolicy.DISABLED)
                .build(),
        )

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
                    poster()
                }
            }
    }

    private class OpenServer : Dispatcher() {
        override fun dispatch(request: RecordedRequest): MockResponse = poster()
    }

    private companion object {
        const val SESSION = "beam_session=s3cret"
        const val TIMEOUT_SECONDS = 5L

        const val SERVER_HOST = "localhost"

        /**
         * Another host than the signed-in server's -- loopback by address, where
         * the server is reached by name -- so the two differ in host, not only in
         * port.
         */
        const val OTHER_HOST = "127.0.0.1"

        /** A 1x1 PNG: the smallest thing the decoder accepts as an image. */
        val POSTER_PNG: ByteArray =
            Base64.decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
                Base64.DEFAULT,
            )

        fun poster(): MockResponse =
            MockResponse
                .Builder()
                .code(200)
                .setHeader("Content-Type", "image/png")
                .body(Buffer().write(POSTER_PNG))
                .build()
    }
}
