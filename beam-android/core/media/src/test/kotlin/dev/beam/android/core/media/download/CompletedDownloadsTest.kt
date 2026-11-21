// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import android.content.Context
import android.database.SQLException
import android.net.ConnectivityManager
import android.net.NetworkCapabilities
import android.net.Uri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.DatabaseIOException
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.cache.CacheWriter
import androidx.media3.exoplayer.offline.Download
import androidx.media3.exoplayer.offline.DownloadCursor
import androidx.media3.exoplayer.offline.DownloadIndex
import androidx.media3.test.utils.robolectric.RobolectricUtil.runMainLooperUntil
import androidx.test.core.app.ApplicationProvider
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.ServerCallFactory
import dev.beam.android.core.media.player.DownloadCacheRule
import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.DownloadState
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
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
import org.junit.Assert.assertFalse
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
import java.util.concurrent.atomic.AtomicReference

/**
 * Which downloads count as complete, and that a finished one stays listed,
 * against a real Media3 download manager and a real server on loopback: the
 * server serves [DONE] in full and refuses
 * every request for [PARTIAL], whose first bytes are already in the cache.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class CompletedDownloadsTest {
    @get:Rule val temp = TemporaryFolder()

    @get:Rule val downloads = DownloadCacheRule()

    private val context: Context = ApplicationProvider.getApplicationContext()
    private val server = MockWebServer()
    private lateinit var repository: MediaDownloadRepository

    @Before
    fun setUp() {
        // As in MediaDownloadRepositoryTest: a validated network, with
        // cellular allowed, so the manager's requirements are met.
        Dispatchers.setMain(UnconfinedTestDispatcher())
        val connectivity = context.getSystemService(ConnectivityManager::class.java)
        val capabilities = ShadowNetworkCapabilities.newInstance()
        shadowOf(capabilities).addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
        shadowOf(capabilities).addCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
        shadowOf(connectivity).setNetworkCapabilities(connectivity.activeNetwork, capabilities)
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse =
                    if (request.url.encodedPath == "/v1/files/$DONE/stream") {
                        MockResponse.Builder().body(BODY).build()
                    } else {
                        MockResponse.Builder().code(503).build()
                    }
            }
        server.start()
        repository =
            MediaDownloadRepository(
                context = context,
                playback = Playback(),
                calls = ServerCallFactory(BeamHttpClientFactory(BeamHttpClientFactory.shared())) { null },
                commands = InProcessDownloadCommands { repository.manager() },
                cache = downloads.cache,
                titles = FileDownloadTitleStore(context),
                artwork = DownloadArtwork(context, temp.newFolder("artwork")) { error("no poster in this test") },
                preferences = FakePreferencesRepository(UserPreferences(downloadOverCellular = true)),
            )
        repository.manager().resumeDownloads()
    }

    @After
    fun tearDown() {
        repository.manager().release()
        server.close()
        Dispatchers.resetMain()
    }

    @Test
    fun `only a download that has completed counts as downloaded`() =
        runTest {
            // The first bytes of PARTIAL, as an interrupted download leaves them.
            CacheWriter(
                CacheDataSource(downloads.cache, ByteArrayDataSource(BODY.toByteArray())),
                DataSpec
                    .Builder()
                    .setUri(Uri.parse(mediaUrl(PARTIAL)))
                    .setLength(4)
                    .build(),
                null,
                null,
            ).cache()

            // DONE first: the manager runs one download at a time, and PARTIAL
            // never finishes.
            enqueue(DONE)
            runMainLooperUntil { stateOf(DONE) == Download.STATE_COMPLETED }
            enqueue(PARTIAL)
            runMainLooperUntil { stateOf(PARTIAL) != null }

            assertTrue(downloads.cache.getCachedSpans(mediaUrl(PARTIAL)).isNotEmpty())
            assertFalse("bytes on disk are not a finished download", repository.isDownloaded(PARTIAL))
            assertNull(repository.completed(PARTIAL))
            assertTrue(repository.isDownloaded(DONE))
            assertEquals(CompletedDownload(DONE, mediaUrl(DONE)), repository.completed(DONE))
            assertNull("never queued", repository.completed("unknown"))
        }

    @Test
    fun `a download stays listed once it has completed or failed`() =
        runTest {
            // Media3 waits seconds between attempts; failing on the first
            // keeps this to the outcome rather than the backoff.
            repository.manager().minRetryCount = 0
            // Atomic: the list is read from the download index on an IO thread.
            val listed = AtomicReference<List<DownloadRecord>>(emptyList())
            backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) {
                repository.downloads.collect(listed::set)
            }

            enqueue(DONE)
            enqueue(PARTIAL)
            runMainLooperUntil {
                val finished = setOf(DownloadState.Completed, DownloadState.Failed)
                listed.get().size == 2 && listed.get().all { it.state in finished }
            }

            assertEquals(
                listOf(DONE to DownloadState.Completed, PARTIAL to DownloadState.Failed),
                listed.get().map { it.fileId to it.state },
            )
        }

    @Test
    fun `an index that cannot be read counts as no completed download`() {
        // What Media3's SQLite index throws when its database cannot be read.
        val unreadable =
            object : DownloadIndex {
                override fun getDownload(id: String): Download? =
                    throw DatabaseIOException(SQLException("disk I/O error"))

                override fun getDownloads(vararg states: Int): DownloadCursor =
                    throw DatabaseIOException(SQLException("disk I/O error"))
            }

        assertNull(unreadable.completedDownload(DONE))
    }

    private suspend fun enqueue(fileId: String) =
        repository.enqueue(fileId = fileId, serverId = "home", mediaId = "m-$fileId", title = "Title $fileId")

    private fun stateOf(fileId: String): Int? =
        repository
            .manager()
            .downloadIndex
            .getDownload(fileId)
            ?.state

    private fun mediaUrl(fileId: String): String = server.url("/v1/files/$fileId/stream").toString()

    /** Media URLs on the loopback server; everything else is the shared fake's. */
    private inner class Playback : PlaybackRepository by FakePlaybackRepository() {
        override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig =
            PlaybackHttpConfig(
                url = mediaUrl(fileId),
                headers = emptyMap(),
                trustedFingerprints = emptyList(),
                pinnedHost = server.hostName,
            )
    }

    private companion object {
        const val DONE = "done"
        const val PARTIAL = "partial"
        const val BODY = "media bytes"
    }
}
