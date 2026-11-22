// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.content.Context
import android.util.Base64
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.datasource.cache.NoOpCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.test.core.app.ApplicationProvider
import coil3.ImageLoader
import coil3.decode.DataSource
import coil3.request.CachePolicy
import coil3.request.ImageRequest
import coil3.request.ImageResult
import coil3.request.SuccessResult
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.BeamImageLoader
import dev.beam.android.core.media.http.ServerCallFactory
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.async
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.GraphicsMode
import uniffi.beam_client_core.BeamClient
import java.io.File
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * A download's poster, from enqueue to removal.
 *
 * Everything below the network is real: Media3's download manager, the core
 * and the records it keeps, the app's own image loader and its disk cache on
 * a temporary directory. Only the wire is faked, because "the device is
 * offline" is the one condition the feature exists for and the one a test
 * cannot otherwise produce. (That the loader authenticates is
 * `BeamImageLoaderTest`'s concern; here no server is signed in, so every
 * fetch goes out on the plain client.)
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
class DownloadArtworkTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val context: Context = ApplicationProvider.getApplicationContext()

    /** The wire. Off means every request fails the way airplane mode does. */
    private var online = true
    private val fetched = mutableListOf<String>()

    /** Set to hold the next response until the test lets it through. */
    private var stall: Stall? = null

    private class Stall {
        val reached = CountDownLatch(1)
        val released = CountDownLatch(1)
    }

    private val http =
        OkHttpClient
            .Builder()
            .addInterceptor { chain ->
                val request = chain.request()
                if (!online) throw IOException("Network is unreachable")
                stall?.let { held ->
                    held.reached.countDown()
                    check(held.released.await(STALL_TIMEOUT_SECONDS, TimeUnit.SECONDS)) { "never released" }
                }
                synchronized(fetched) { fetched += request.url.toString() }
                Response
                    .Builder()
                    .request(request)
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .body(POSTER_PNG.toResponseBody("image/png".toMediaType()))
                    .build()
            }.build()

    private lateinit var loader: ImageLoader
    private lateinit var database: StandaloneDatabaseProvider
    private lateinit var cache: SimpleCache
    private lateinit var media3: DownloadManager
    private lateinit var core: BeamClient
    private lateinit var artwork: DownloadArtwork
    private lateinit var downloads: BeamDownloadManager

    @Before
    fun setUp() {
        // Coil hops to the main dispatcher for its interceptor chain, and under
        // Robolectric that is the paused main looper this test runs on.
        Dispatchers.setMain(UnconfinedTestDispatcher())
        loader =
            BeamImageLoader.build(
                context = context,
                calls = ServerCallFactory(BeamHttpClientFactory(http)) { null },
                diskCacheDirectory = temp.newFolder("artwork"),
            )
        database = StandaloneDatabaseProvider(context)
        cache = SimpleCache(temp.newFolder("downloads"), NoOpCacheEvictor(), database)
        media3 =
            DownloadManager(
                context,
                database,
                cache,
                DefaultHttpDataSource.Factory(),
                Executors.newSingleThreadExecutor(),
            )
        // Only the artwork is under test; the media bytes are not fetched.
        media3.pauseDownloads()
        core = BeamClient(InMemoryKeyValueStore())
        artwork = DownloadArtwork(context, temp.newFolder("download-artwork")) { loader }
        downloads = BeamDownloadManager(media3, InProcessDownloadCommands { media3 }, core, artwork)
    }

    @After
    fun tearDown() {
        media3.release()
        cache.release()
        database.close()
        core.close()
        loader.shutdown()
        Dispatchers.resetMain()
    }

    @Test
    fun `a poster fetched at enqueue renders with the network gone`() =
        runTest {
            enqueue(F1, POSTER)

            online = false
            val result = renderRecordOf(F1)

            assertTrue("rendered offline, got $result", result is SuccessResult)
            assertEquals(DataSource.DISK, (result as SuccessResult).dataSource)
            assertEquals(listOf(POSTER), fetched)
        }

    @Test
    fun `a kept poster outlives the image cache dropping it`() =
        runTest {
            enqueue(F1, POSTER)

            online = false
            // What enough browsing does to an LRU cache, all at once.
            loader.diskCache!!.clear()
            val result = renderRecordOf(F1)

            assertTrue("rendered offline from the kept copy, got $result", result is SuccessResult)
            assertEquals(listOf(POSTER), fetched)
        }

    @Test
    fun `the record names a path on disk, never the poster's URL`() =
        runTest {
            enqueue(F1, POSTER)

            val path = recordOf(F1).artworkPath!!
            assertEquals(artwork.pathFor(POSTER), path)
            assertTrue(File(path).isFile)
        }

    @Test
    fun `a poster that could not be fetched is fetched on the next enqueue`() =
        runTest {
            online = false
            // A poster is decoration; losing it must not lose the download.
            enqueue(F1, POSTER)
            assertEquals("the download was still recorded", listOf(F1), core.downloads().map { it.fileId })
            assertFalse(isOnDisk(POSTER))

            online = true
            enqueue(F1, POSTER)

            assertTrue(isOnDisk(POSTER))
        }

    @Test
    fun `a poster already kept is not fetched again`() =
        runTest {
            enqueue(E1, POSTER)
            // Without the kept copy, a second pin would have to go back to the wire.
            loader.diskCache!!.clear()

            enqueue(E2, POSTER)

            assertEquals(listOf(POSTER), fetched)
            assertTrue(isOnDisk(POSTER))
        }

    @Test
    fun `a poster that cannot be stored does not fail the download`() =
        runTest {
            // A regular file where the store's directory should be: every write into it fails.
            val blocked = temp.newFile("not-a-directory")
            artwork = DownloadArtwork(context, blocked) { loader }
            downloads = BeamDownloadManager(media3, InProcessDownloadCommands { media3 }, core, artwork)

            enqueue(F1, POSTER)

            assertEquals("the download was still recorded", listOf(F1), core.downloads().map { it.fileId })
            assertEquals(listOf(POSTER), fetched)
            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `removing the download releases its poster`() =
        runTest {
            enqueue(F1, POSTER)
            assertTrue(isOnDisk(POSTER))

            downloads.remove(F1)

            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `a poster shared by two downloads is kept until the last is removed`() =
        runTest {
            // Two episodes of one series carry the series poster.
            enqueue(E1, POSTER)
            enqueue(E2, POSTER)

            downloads.remove(E1)
            assertTrue("still needed by e2", isOnDisk(POSTER))

            downloads.remove(E2)
            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `removing a download leaves other downloads' posters alone`() =
        runTest {
            enqueue(F1, POSTER)
            enqueue(F2, OTHER_POSTER)

            downloads.remove(F1)

            assertFalse(isOnDisk(POSTER))
            assertTrue(isOnDisk(OTHER_POSTER))
        }

    @Test
    fun `removing every download releases every poster`() =
        runTest {
            enqueue(F1, POSTER)
            enqueue(F2, OTHER_POSTER)
            assertTrue(isOnDisk(POSTER) && isOnDisk(OTHER_POSTER))

            downloads.removeAll()

            assertFalse(isOnDisk(POSTER))
            assertFalse(isOnDisk(OTHER_POSTER))
        }

    @Test
    fun `a poster shared with a download cleared by remove-all is released with the last`() =
        runTest {
            enqueue(E1, POSTER)
            downloads.removeAll()

            // A later episode of the same series brings the poster back ...
            enqueue(E2, POSTER)
            assertTrue(isOnDisk(POSTER))

            // ... and removing it must not find e1 still holding it.
            downloads.remove(E2)

            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `forgetting a server releases the posters only its downloads kept`() =
        runTest {
            enqueue(H1, POSTER, server = "https://home.test")
            enqueue(C1, POSTER, server = "https://cabin.test")
            enqueue(C2, OTHER_POSTER, server = "https://cabin.test")

            downloads.removeServer(core.addServer("https://cabin.test", null).id)

            assertTrue("still shown by the other server's download", isOnDisk(POSTER))
            assertFalse(isOnDisk(OTHER_POSTER))
            assertEquals(listOf(H1), core.downloads().map { it.fileId })
        }

    @Test
    fun `a download removed while its poster is still fetching leaves no poster behind`() =
        runTest {
            val held = Stall().also { stall = it }
            val enqueueing = async(Dispatchers.Default) { enqueue(F1, POSTER) }
            assertTrue(
                "the poster fetch reached the wire",
                held.reached.await(STALL_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )

            // Removed before the poster's bytes have landed: nothing to delete yet.
            downloads.remove(F1)
            held.released.countDown()
            enqueueing.await()

            assertFalse(isOnDisk(POSTER))
        }

    /** Queue [fileId] on the server at [server], made the active one. */
    private suspend fun enqueue(
        fileId: String,
        posterUrl: String,
        server: String = "https://beam.test",
    ) {
        core.selectServer(core.addServer(server, null).id)
        downloads.enqueue(
            fileId = fileId,
            mediaId = "m-$fileId",
            episodeId = null,
            title = "Title $fileId",
            subtitle = null,
            posterUrl = posterUrl,
        )
    }

    /** The record the downloads screen would be handed for [fileId]. */
    private suspend fun recordOf(fileId: String) = core.downloads().single { it.fileId == fileId }

    /**
     * Render [fileId]'s poster the way the downloads screen does: the record's
     * kept copy as the model, default keys. The memory cache is bypassed
     * because a fresh process has none.
     */
    private suspend fun renderRecordOf(fileId: String): ImageResult =
        loader.execute(
            ImageRequest
                .Builder(context)
                .data(File(recordOf(fileId).artworkPath!!).toURI().toString())
                .memoryCachePolicy(CachePolicy.DISABLED)
                .build(),
        )

    /** Whether a copy of [url] is kept on disk. */
    private fun isOnDisk(url: String): Boolean = File(artwork.pathFor(url)).isFile

    private companion object {
        const val POSTER = "https://beam.test/v1/artwork/media/m1/poster"
        const val OTHER_POSTER = "https://beam.test/v1/artwork/media/m2/poster"
        const val STALL_TIMEOUT_SECONDS = 10L
        const val F1 = "f1000000-0000-4000-8000-000000000001"
        const val F2 = "f2000000-0000-4000-8000-000000000002"
        const val E1 = "e1000000-0000-4000-8000-000000000001"
        const val E2 = "e2000000-0000-4000-8000-000000000002"
        const val H1 = "a1000000-0000-4000-8000-000000000001"
        const val C1 = "c1000000-0000-4000-8000-000000000001"
        const val C2 = "c2000000-0000-4000-8000-000000000002"

        /** A 1x1 PNG: the smallest thing the decoder accepts as an image. */
        val POSTER_PNG: ByteArray =
            Base64.decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
                Base64.DEFAULT,
            )
    }
}
