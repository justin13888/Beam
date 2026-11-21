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
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.ServerCallFactory
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakePreferencesRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import org.junit.After
import org.junit.Before
import org.junit.Rule
import org.junit.rules.TemporaryFolder
import org.robolectric.Shadows.shadowOf
import org.robolectric.shadows.ShadowNetworkCapabilities
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.DownloadRecord
import uniffi.beam_client_core.ServerHttpConfig
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeoutException

/**
 * The downloads repository as the app builds it, against a real Media3
 * download manager, the real core, and a real server on loopback.
 *
 * Below the trait line: the core's storage, held in memory; each server's
 * session, which is whatever [sessions] holds for it, in place of the
 * core's answer; and the service start, which [InProcessDownloadCommands]
 * stands in for. The servers themselves are the core's own list. No server
 * is signed in to the core unless a test signs one in, answering `/v1/me`
 * through [canned]; without that, or without a sources listing there, the
 * core plans each file alone, from the server's download route.
 *
 * Every subclass runs under `@GraphicsMode(NATIVE)`, as `DownloadArtworkTest`
 * does, so that every test here that builds a core shares one Robolectric
 * sandbox. The native core is loaded once per JVM, and each sandbox that
 * loads the bindings registers its own key-value store callbacks with it,
 * replacing the last sandbox's: a core built in any other sandbox then calls
 * back into handles that sandbox never issued, and its calls never return.
 */
@OptIn(ExperimentalCoroutinesApi::class)
abstract class DownloadRepositoryHarness {
    @get:Rule
    val temp = TemporaryFolder()

    private val context: Context = ApplicationProvider.getApplicationContext()

    /** The signed-in server: the bytes for [FRESH] alone, 401 for anyone else. */
    private val beam = MockWebServer()

    /** Each signed-in server's session, by the core's server id. */
    protected val sessions = ConcurrentHashMap<String, ServerHttpConfig>()

    /** The `Cookie` of every request for bytes the server answered, in order. */
    protected val cookies = mutableListOf<String?>()

    /** JSON the server answers the core with, by request path, whatever the session. */
    protected val canned = ConcurrentHashMap<String, String>()

    /** Run on the server's thread whenever it refuses a request. */
    @Volatile
    protected var onRefused: () -> Unit = {}

    /** What the repository last listed. */
    @Volatile
    protected var listed: List<DownloadRecord> = emptyList()

    private lateinit var database: StandaloneDatabaseProvider
    protected lateinit var cache: SimpleCache
    protected lateinit var core: BeamClient

    // Internal rather than protected: the repository's type is internal.
    internal lateinit var repository: MediaDownloadRepository

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
        core = BeamClient(InMemoryKeyValueStore())
        repository =
            MediaDownloadRepository(
                context = context,
                core = core,
                calls =
                    ServerCallFactory.signedInServers(
                        BeamHttpClientFactory(BeamHttpClientFactory.shared()),
                        servers = { core.listServers() },
                    ) { serverId -> sessions[serverId] },
                commands = InProcessDownloadCommands { repository.manager() },
                cache = cache,
                artwork = DownloadArtwork(context, temp.newFolder("artwork")) { error("no poster in this test") },
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
        core.close()
        beam.close()
        Dispatchers.resetMain()
    }

    /** Collect the repository's list into [listed], as the downloads screen would. */
    protected fun TestScope.observe() {
        backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
            repository.downloads.collect { listed = it }
        }
    }

    /**
     * Run the main looper, which Media3 reports every download change on,
     * until [condition] holds; on a timeout, fail saying where each download
     * got to and what the server was sent.
     */
    protected fun awaitUntil(condition: () -> Boolean) {
        try {
            runMainLooperUntil { condition() }
        } catch (timeout: TimeoutException) {
            val downloads = indexed().map { "${it.request.id}: state ${it.state}, stop ${it.stopReason}" }
            val manager = repository.manager()
            throw AssertionError(
                "paused ${manager.downloadsPaused}, unmet requirements ${manager.notMetRequirements}, " +
                    "downloads $downloads, listed ${listed.map { it.fileId to it.state }}, " +
                    "cookies ${synchronized(cookies) { cookies.toList() }}",
                timeout,
            )
        }
    }

    /** Queue [fileId] with [on] as the active server, which is where the core queues it. */
    protected suspend fun enqueue(
        fileId: String,
        on: String,
        mediaId: String = "m-$fileId",
    ) {
        core.selectServer(serverId(on))
        repository.enqueue(fileId = fileId, mediaId = mediaId, title = "Title $fileId")
    }

    /** The core's id for the server at [origin], registering it on first use. */
    protected suspend fun serverId(origin: String): String = core.addServer(origin(origin), null).id

    /**
     * Every download in the manager's index, finished ones included: the
     * manager's own `currentDownloads` leaves out a completed download.
     */
    protected fun indexed(): List<Download> = repository.manager().indexedDownloads()

    protected fun stateOf(fileId: String) = listed.firstOrNull { it.fileId == fileId }?.state

    /** The session [cookie] on the server named [server]. */
    protected fun config(
        cookie: String,
        server: String = HOME,
    ) = ServerHttpConfig(
        baseUrl = origin(server),
        headers = mapOf("Cookie" to cookie),
        trustedFingerprints = emptyList(),
        host = server,
    )

    /**
     * The loopback server under one of two names, so the core registers two
     * servers that answer from the same place.
     */
    private fun origin(name: String): String = "http://$name:${beam.port}"

    /**
     * Where [fileId]'s bytes are served -- and so the key Media3 caches them
     * under, since a download request carries no custom cache key.
     */
    protected fun mediaUrl(
        fileId: String,
        server: String,
    ): String = "${origin(server)}/v1/files/$fileId/download"

    private inner class BeamServer : Dispatcher() {
        override fun dispatch(request: RecordedRequest): MockResponse {
            canned[request.url.encodedPath]?.let { listing ->
                return MockResponse
                    .Builder()
                    .addHeader("Content-Type", "application/json")
                    .body(listing)
                    .build()
            }
            val cookie = request.headers["Cookie"]
            synchronized(cookies) { cookies += cookie }
            if (cookie != FRESH) {
                onRefused()
                return MockResponse.Builder().code(401).build()
            }
            return MockResponse.Builder().body(BODY).build()
        }
    }

    protected companion object {
        const val STALE = "beam_session=stale"
        const val FRESH = "beam_session=fresh"
        const val BODY = "media bytes"
        const val HOME = "localhost"
        const val CABIN = "127.0.0.1"
    }
}
