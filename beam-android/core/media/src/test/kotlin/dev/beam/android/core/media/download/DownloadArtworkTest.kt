// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.content.Context
import android.util.Base64
import androidx.core.net.toUri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.datasource.cache.NoOpCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadManager
import androidx.media3.exoplayer.offline.DownloadRequest
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
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.testing.FakePlaybackRepository
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
import java.io.IOException
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/**
 * A download's poster, from enqueue to removal.
 *
 * Everything below the network is real: Media3's download manager, the title
 * store, the app's own image loader and its disk cache on a temporary
 * directory. Only the wire is faked, because "the device is offline" is the
 * one condition the feature exists for and the one a test cannot otherwise
 * produce. (That the loader authenticates is `BeamImageLoaderTest`'s concern;
 * here no server is signed in, so every fetch goes out on the plain client.)
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
    private lateinit var titles: DownloadTitleStore
    private lateinit var artwork: DownloadArtwork
    private lateinit var downloads: BeamDownloadManager
    private val playback = FakePlaybackRepository()

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
        titles = FileDownloadTitleStore(context)
        artwork = DownloadArtwork(context, temp.newFolder("download-artwork")) { loader }
        downloads = BeamDownloadManager(media3, titles, artwork)
    }

    @After
    fun tearDown() {
        media3.release()
        cache.release()
        database.close()
        loader.shutdown()
        Dispatchers.resetMain()
    }

    @Test
    fun `a poster fetched at enqueue renders with the network gone`() =
        runTest {
            enqueue("f1", POSTER)

            online = false
            val result = renderRecordOf("f1")

            assertTrue("rendered offline, got $result", result is SuccessResult)
            assertEquals(DataSource.DISK, (result as SuccessResult).dataSource)
            assertEquals(listOf(POSTER), fetched)
        }

    @Test
    fun `a kept poster outlives the image cache dropping it`() =
        runTest {
            enqueue("f1", POSTER)

            online = false
            // What enough browsing does to an LRU cache, all at once.
            loader.diskCache!!.clear()
            val result = renderRecordOf("f1")

            assertTrue("rendered offline from the kept copy, got $result", result is SuccessResult)
            assertEquals(listOf(POSTER), fetched)
        }

    @Test
    fun `a download whose poster could not be kept still names the poster's URL`() =
        runTest {
            online = false
            enqueue("f1", POSTER)

            // Online, the screen still fetches it like any other poster.
            assertEquals(POSTER, recordOf("f1").posterUrl)
        }

    @Test
    fun `a poster that could not be fetched is fetched on the next enqueue`() =
        runTest {
            online = false
            // A poster is decoration; losing it must not lose the download.
            enqueue("f1", POSTER)
            assertTrue("the download was still recorded", titles.get("f1") != null)
            assertFalse(isOnDisk(POSTER))

            online = true
            enqueue("f1", POSTER)

            assertTrue(isOnDisk(POSTER))
        }

    @Test
    fun `a poster already kept is not fetched again`() =
        runTest {
            enqueue("e1", POSTER)
            // Without the kept copy, a second pin would have to go back to the wire.
            loader.diskCache!!.clear()

            enqueue("e2", POSTER)

            assertEquals(listOf(POSTER), fetched)
            assertTrue(isOnDisk(POSTER))
        }

    @Test
    fun `a poster that cannot be stored does not fail the download`() =
        runTest {
            // A regular file where the store's directory should be: every write into it fails.
            val blocked = temp.newFile("not-a-directory")
            artwork = DownloadArtwork(context, blocked) { loader }
            downloads = BeamDownloadManager(media3, titles, artwork)

            enqueue("f1", POSTER)

            assertTrue("the download was still recorded", titles.get("f1") != null)
            assertEquals(listOf(POSTER), fetched)
            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `removing the download releases its poster`() =
        runTest {
            enqueue("f1", POSTER)
            assertTrue(isOnDisk(POSTER))

            downloads.remove("f1")

            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `a poster shared by two downloads is kept until the last is removed`() =
        runTest {
            // Two episodes of one series carry the series poster.
            enqueue("e1", POSTER)
            enqueue("e2", POSTER)

            downloads.remove("e1")
            assertTrue("still needed by e2", isOnDisk(POSTER))

            downloads.remove("e2")
            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `removing a download leaves other downloads' posters alone`() =
        runTest {
            enqueue("f1", POSTER)
            enqueue("f2", OTHER_POSTER)

            downloads.remove("f1")

            assertFalse(isOnDisk(POSTER))
            assertTrue(isOnDisk(OTHER_POSTER))
        }

    @Test
    fun `removing every download releases every poster`() =
        runTest {
            enqueue("f1", POSTER)
            enqueue("f2", OTHER_POSTER)
            assertTrue(isOnDisk(POSTER) && isOnDisk(OTHER_POSTER))

            downloads.removeAll()

            assertFalse(isOnDisk(POSTER))
            assertFalse(isOnDisk(OTHER_POSTER))
        }

    @Test
    fun `a poster shared with a download cleared by remove-all is released with the last`() =
        runTest {
            enqueue("e1", POSTER)
            downloads.removeAll()

            // A later episode of the same series brings the poster back ...
            enqueue("e2", POSTER)
            assertTrue(isOnDisk(POSTER))

            // ... and removing it must not find e1 still holding it.
            downloads.remove("e2")

            assertFalse(isOnDisk(POSTER))
        }

    @Test
    fun `a download removed while its poster is still fetching leaves no poster behind`() =
        runTest {
            val held = Stall().also { stall = it }
            val enqueueing = async(Dispatchers.Default) { enqueue("f1", POSTER) }
            assertTrue(
                "the poster fetch reached the wire",
                held.reached.await(STALL_TIMEOUT_SECONDS, TimeUnit.SECONDS),
            )

            // Removed before the poster's bytes have landed: nothing to evict yet.
            downloads.remove("f1")
            held.released.countDown()
            enqueueing.await()

            assertFalse(isOnDisk(POSTER))
        }

    private suspend fun enqueue(
        fileId: String,
        posterUrl: String,
    ) = downloads.enqueue(
        fileId = fileId,
        serverId = "s1",
        mediaId = "m-$fileId",
        title = "Title $fileId",
        subtitle = null,
        posterUrl = posterUrl,
        repository = playback,
    )

    /** The record the downloads screen would be handed for [fileId]. */
    private fun recordOf(fileId: String): DownloadRecord =
        Download(
            DownloadRequest.Builder(fileId, "https://beam.test/v1/files/$fileId".toUri()).build(),
            Download.STATE_COMPLETED,
            0L,
            0L,
            0L,
            Download.STOP_REASON_NONE,
            Download.FAILURE_REASON_NONE,
        ).toRecord(titles, artwork)

    /**
     * Render [fileId]'s poster the way `Artwork` does: the record's poster as
     * the model, default keys. The memory cache is bypassed because a fresh
     * process has none.
     */
    private suspend fun renderRecordOf(fileId: String): ImageResult =
        loader.execute(
            ImageRequest
                .Builder(context)
                .data(recordOf(fileId).posterUrl)
                .memoryCachePolicy(CachePolicy.DISABLED)
                .build(),
        )

    /** Whether the downloads screen would render [url] from a kept copy. */
    private fun isOnDisk(url: String): Boolean = artwork.displayUrl(url) != url

    private companion object {
        const val POSTER = "https://beam.test/v1/artwork/media/m1/poster"
        const val OTHER_POSTER = "https://beam.test/v1/artwork/media/m2/poster"
        const val STALL_TIMEOUT_SECONDS = 10L

        /** A 1x1 PNG: the smallest thing the decoder accepts as an image. */
        val POSTER_PNG: ByteArray =
            Base64.decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
                Base64.DEFAULT,
            )
    }
}
