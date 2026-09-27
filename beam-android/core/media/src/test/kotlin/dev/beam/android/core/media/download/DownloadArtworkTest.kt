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
import coil3.disk.DiskCache
import coil3.network.okhttp.OkHttpNetworkFetcherFactory
import coil3.request.CachePolicy
import coil3.request.ImageRequest
import coil3.request.SuccessResult
import dev.beam.android.core.testing.FakePlaybackRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Path.Companion.toOkioPath
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
import java.util.concurrent.Executors

/**
 * A download's poster, from enqueue to removal.
 *
 * Everything below the network is real: Media3's download manager, the title
 * store, Coil's loader and its disk cache on a temporary directory. Only the
 * wire is faked, because "the device is offline" is the one condition the
 * feature exists for and the one a test cannot otherwise produce.
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

    private val http =
        OkHttpClient
            .Builder()
            .addInterceptor { chain ->
                val request = chain.request()
                if (!online) throw IOException("Network is unreachable")
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
    private lateinit var downloads: BeamDownloadManager
    private val playback = FakePlaybackRepository()

    @Before
    fun setUp() {
        // Coil hops to the main dispatcher for its interceptor chain, and under
        // Robolectric that is the paused main looper this test runs on.
        Dispatchers.setMain(UnconfinedTestDispatcher())
        loader =
            ImageLoader
                .Builder(context)
                .components { add(OkHttpNetworkFetcherFactory(callFactory = { http })) }
                .diskCache {
                    DiskCache
                        .Builder()
                        .directory(temp.newFolder("artwork").toOkioPath())
                        .build()
                }.build()
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
        downloads = BeamDownloadManager(media3, titles, DownloadArtwork(context) { loader })
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
            // What Artwork asks for: the URL as the model, default keys. The
            // memory cache is bypassed because a fresh process has none.
            val result =
                loader.execute(
                    ImageRequest
                        .Builder(context)
                        .data(POSTER)
                        .memoryCachePolicy(CachePolicy.DISABLED)
                        .build(),
                )

            assertTrue("rendered offline, got $result", result is SuccessResult)
            assertEquals(DataSource.DISK, (result as SuccessResult).dataSource)
            assertEquals(listOf(POSTER), fetched)
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

    private fun isOnDisk(url: String): Boolean = loader.diskCache!!.openSnapshot(url)?.use { true } ?: false

    private companion object {
        const val POSTER = "https://beam.test/v1/artwork/media/m1/poster"
        const val OTHER_POSTER = "https://beam.test/v1/artwork/media/m2/poster"

        /** A 1x1 PNG: the smallest thing the decoder accepts as an image. */
        val POSTER_PNG: ByteArray =
            Base64.decode(
                "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8DwHwAFBQIAX8jx0gAAAABJRU5ErkJggg==",
                Base64.DEFAULT,
            )
    }
}
