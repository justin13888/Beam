// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.content.Context
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.cache.NoOpCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import androidx.media3.exoplayer.offline.Download
import androidx.media3.test.utils.robolectric.RobolectricUtil.runMainLooperUntil
import androidx.test.core.app.ApplicationProvider
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.ServerCallFactory
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Shadows.shadowOf
import org.robolectric.shadows.ShadowNetworkCapabilities
import uniffi.beam_client_core.PlaybackHttpConfig
import uniffi.beam_client_core.ServerHttpConfig
import java.util.concurrent.TimeoutException

/**
 * The downloads repository as the app builds it, against a real Media3
 * download manager and a real server on loopback.
 *
 * Below the trait line: the core's list of known servers -- [beam] as `home`,
 * and `cabin`, which is the active one -- its answer for each server's
 * session, which is whatever [home] and [cabin] hold, and the service start,
 * which [InProcessDownloadCommands] stands in for.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class MediaDownloadRepositoryTest {
    @get:Rule
    val temp = TemporaryFolder()

    private val context: Context = ApplicationProvider.getApplicationContext()

    /** The signed-in server: the bytes for [FRESH] alone, 401 for anyone else. */
    private val beam = MockWebServer()

    /** The core's answer for [beam]'s session: `null` while signed out. */
    @Volatile
    private var home: ServerHttpConfig? = null

    /** The core's answer for the other server's session: `null` while signed out. */
    @Volatile
    private var cabin: ServerHttpConfig? = null

    /** The `Cookie` of every request the server answered, in order. */
    private val cookies = mutableListOf<String?>()

    /** Run on the server's thread whenever it refuses a request. */
    @Volatile
    private var onRefused: () -> Unit = {}

    private lateinit var database: StandaloneDatabaseProvider
    private lateinit var cache: SimpleCache
    private lateinit var titles: DownloadTitleStore
    private lateinit var artwork: DownloadArtwork
    private lateinit var repository: MediaDownloadRepository

    @Before
    fun setUp() {
        // The repository follows the cellular preference on the main
        // dispatcher. Cellular is allowed here, so any validated network
        // meets the requirement; Robolectric's default network is connected
        // but not validated, which Media3 reads as no internet.
        Dispatchers.setMain(UnconfinedTestDispatcher())
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        val capabilities = ShadowNetworkCapabilities.newInstance()
        shadowOf(capabilities).addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        shadowOf(capabilities).addCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
        shadowOf(connectivity).setNetworkCapabilities(connectivity.activeNetwork, capabilities)
        beam.dispatcher = BeamServer()
        beam.start()
        database = StandaloneDatabaseProvider(context)
        cache = SimpleCache(temp.newFolder("downloads"), NoOpCacheEvictor(), database)
        titles = FileDownloadTitleStore(context)
        artwork = DownloadArtwork(context, temp.newFolder("artwork")) { error("no poster in this test") }
        repository =
            MediaDownloadRepository(
                context = context,
                playback = Playback(),
                calls =
                    ServerCallFactory.signedInServers(
                        BeamHttpClientFactory(BeamHttpClientFactory.shared()),
                        servers = {
                            listOf(
                                Fixtures.server(id = "home", isActive = false).copy(baseUrl = origin()),
                                Fixtures.server(id = "cabin", isActive = true).copy(baseUrl = CABIN_ORIGIN),
                            )
                        },
                    ) { serverId ->
                        when (serverId) {
                            "home" -> home
                            "cabin" -> cabin
                            else -> null
                        }
                    },
                commands = InProcessDownloadCommands { repository.manager() },
                cache = cache,
                titles = titles,
                artwork = artwork,
                preferences = FakePreferencesRepository(UserPreferences(downloadOverCellular = true)),
            )
        // A Media3 manager starts paused, and the download service resumes
        // it as the service starts. The service does not run here, so this
        // does what its start would.
        repository.manager().resumeDownloads()
    }

    @After
    fun tearDown() {
        repository.manager().release()
        cache.release()
        database.close()
        beam.close()
        Dispatchers.resetMain()
    }

    @Test
    fun `downloads are listed while signed out`() =
        runTest {
            home = null
            val listed = mutableListOf<List<DownloadRecord>>()
            backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
                repository.downloads.collect { listed += it }
            }

            enqueue("f1", serverId = "home")
            awaitUntil { listed.lastOrNull().orEmpty().isNotEmpty() }

            assertEquals(listOf("f1"), listed.last().map(DownloadRecord::fileId))
        }

    @Test
    fun `a download picks up a new session on its next request without the manager being rebuilt`() =
        runTest {
            home = config(STALE)
            // The viewer signs in again while the first attempt is in flight.
            onRefused = { home = config(FRESH) }

            enqueue("f1", serverId = "home")
            awaitUntil { stateOf("f1") == DownloadState.Completed }

            assertEquals(listOf<String?>(STALE, FRESH), synchronized(cookies) { cookies.toList() })
        }

    @Test
    fun `a download from a server that is not the active one completes under its own server's session`() =
        runTest {
            // Queued from home, then the viewer switched to signed-in cabin.
            home = config(FRESH)
            cabin =
                ServerHttpConfig(
                    baseUrl = CABIN_ORIGIN,
                    headers = mapOf("Cookie" to "beam_session=cabin"),
                    trustedFingerprints = emptyList(),
                    host = "cabin.beam.test",
                )

            enqueue("f1", serverId = "home")
            awaitUntil { stateOf("f1") == DownloadState.Completed }

            assertEquals(listOf<String?>(FRESH), synchronized(cookies) { cookies.toList() })
        }

    @Test
    fun `forgetting a server deletes its downloads and leaves other servers' alone`() =
        runTest {
            home = config(FRESH)
            enqueue("h1", serverId = "home")
            enqueue("c1", serverId = "cabin")
            awaitUntil {
                stateOf("h1") == DownloadState.Completed && stateOf("c1") == DownloadState.Completed
            }

            repository.removeServer("cabin")
            awaitUntil { stateOf("c1") == null }

            assertEquals(listOf("h1"), current().map(DownloadRecord::fileId))
            assertEquals(listOf("h1"), titles.all().map(DownloadTitle::fileId))
            assertNull(titles.get("c1"))
            assertTrue("the removed download's bytes are gone", cache.getCachedSpans(mediaUrl("c1")).isEmpty())
            assertTrue("the kept download's bytes stay", cache.getCachedSpans(mediaUrl("h1")).isNotEmpty())
        }

    /**
     * Run the main looper, which Media3 reports every download change on,
     * until [condition] holds; on a timeout, fail saying where each download
     * got to and what the server was sent.
     */
    private fun awaitUntil(condition: () -> Boolean) {
        try {
            runMainLooperUntil { condition() }
        } catch (timeout: TimeoutException) {
            val downloads = indexed().map { "${it.request.id}: state ${it.state}" }
            val manager = repository.manager()
            throw AssertionError(
                "paused ${manager.downloadsPaused}, unmet requirements ${manager.notMetRequirements}, " +
                    "downloads $downloads, cookies ${synchronized(cookies) { cookies.toList() }}",
                timeout,
            )
        }
    }

    private suspend fun enqueue(
        fileId: String,
        serverId: String,
    ) = repository.enqueue(
        fileId = fileId,
        serverId = serverId,
        mediaId = "m-$fileId",
        title = "Title $fileId",
        subtitle = null,
        posterUrl = null,
    )

    /**
     * Every download in the manager's index, finished ones included: the
     * manager's own `currentDownloads` leaves out a completed download.
     */
    private fun indexed(): List<Download> =
        repository.manager().downloadIndex.getDownloads().use { cursor ->
            buildList { while (cursor.moveToNext()) add(cursor.download) }
        }

    private fun current(): List<DownloadRecord> = indexed().map { it.toRecord(titles, artwork) }

    private fun stateOf(fileId: String): DownloadState? = current().firstOrNull { it.fileId == fileId }?.state

    /** [beam]'s session, holding [cookie]. */
    private fun config(cookie: String): ServerHttpConfig =
        ServerHttpConfig(
            baseUrl = origin(),
            headers = mapOf("Cookie" to cookie),
            trustedFingerprints = emptyList(),
            host = beam.url("/").host,
        )

    /** [beam]'s origin, normalised as the core records a server's. */
    private fun origin(): String = beam.url("/").toString().trimEnd('/')

    /**
     * Where [fileId]'s bytes are served -- and so the key Media3 caches them
     * under, since a download request carries no custom cache key.
     */
    private fun mediaUrl(fileId: String): String = beam.url("/v1/files/$fileId/stream").toString()

    /** Media URLs on the loopback server; everything else is the shared fake's. */
    private inner class Playback : PlaybackRepository by FakePlaybackRepository() {
        override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig =
            PlaybackHttpConfig(
                url = mediaUrl(fileId),
                headers = emptyMap(),
                trustedFingerprints = emptyList(),
                pinnedHost = beam.hostName,
            )
    }

    private inner class BeamServer : Dispatcher() {
        override fun dispatch(request: RecordedRequest): MockResponse {
            val cookie = request.headers["Cookie"]
            synchronized(cookies) { cookies += cookie }
            if (cookie != FRESH) {
                onRefused()
                return MockResponse.Builder().code(401).build()
            }
            return MockResponse.Builder().body(BODY).build()
        }
    }

    private companion object {
        const val STALE = "beam_session=stale"
        const val FRESH = "beam_session=fresh"
        const val BODY = "media bytes"

        /** The other server, which no request in these tests is sent to. */
        const val CABIN_ORIGIN = "https://cabin.beam.test"
    }
}
