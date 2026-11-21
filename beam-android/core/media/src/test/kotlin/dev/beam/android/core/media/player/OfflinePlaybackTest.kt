// Media3's test players and cache are @UnstableApi, like the ExoPlayer surface itself.
@file:UnstableApi

package dev.beam.android.core.media.player

import androidx.media3.common.Player
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.test.utils.TestExoPlayerBuilder
import androidx.media3.test.utils.robolectric.TestPlayerRunHelper
import dev.beam.android.core.media.download.CompletedDownload
import dev.beam.android.core.media.download.CompletedDownloads
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakeTelemetryRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import mockwebserver3.Dispatcher
import mockwebserver3.MockResponse
import mockwebserver3.MockWebServer
import mockwebserver3.RecordedRequest
import okio.Buffer
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.beam_client_core.BeamException
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * Playback through the download cache, over a real ExoPlayer and a real
 * server on loopback.
 *
 * The media is a short WAV file, which the player's own extractor parses, so
 * reaching [Player.STATE_READY] means the bytes were read and understood.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class OfflinePlaybackTest {
    @get:Rule val downloads = DownloadCacheRule()

    private val exoPlayer: ExoPlayer = TestExoPlayerBuilder(RuntimeEnvironment.getApplication()).build()
    private val playback = FakePlaybackRepository()

    /** The server: [WAV] to anyone while [isUp], 503 otherwise; it records every request. */
    private val server = MockWebServer()

    @Volatile
    private var isUp = false

    @Before
    fun setUp() {
        server.dispatcher =
            object : Dispatcher() {
                override fun dispatch(request: RecordedRequest): MockResponse =
                    if (isUp) {
                        MockResponse.Builder().body(Buffer().write(WAV)).build()
                    } else {
                        MockResponse.Builder().code(503).build()
                    }
            }
        server.start()
    }

    @After
    fun tearDown() {
        exoPlayer.release()
        server.close()
    }

    private fun player(
        scope: TestScope,
        completed: CompletedDownloads,
    ) = ExoBeamPlayer(
        context = RuntimeEnvironment.getApplication(),
        exoPlayer = exoPlayer,
        repository = playback,
        telemetry = FakeTelemetryRepository(),
        clients = BeamHttpClientFactory(BeamHttpClientFactory.shared()),
        downloadCache = downloads.cache,
        completedDownloads = completed,
        scope = scope,
    )

    @Test
    fun `a completed download plays from disk with no session and no server`() =
        runTest {
            val scope = TestScope(testScheduler)
            val uri = mediaUrl()
            downloads.store(uri, WAV)
            // Signed out, or the session expired: the core cannot plan anything.
            playback.failWith = BeamException.Unauthenticated()
            val player = player(scope) { fileId -> CompletedDownload(fileId, uri).takeIf { fileId == FILE_ID } }

            player.play(REQUEST)
            scope.runCurrent()
            TestPlayerRunHelper.run(exoPlayer).untilState(Player.STATE_READY)

            assertTrue("the core was never asked for a plan", playback.planRequests.isEmpty())
            assertEquals(
                uri,
                exoPlayer
                    .getMediaItemAt(0)
                    .localConfiguration
                    ?.uri
                    .toString(),
            )
            assertEquals(FILE_ID, exoPlayer.getMediaItemAt(0).mediaId)
            assertTrue("bytes were read", exoPlayer.bufferedPosition > 0L)
            assertEquals("every byte came from the cache", 0, server.requestCount)
            player.stop()
        }

    @Test
    fun `a streamed file whose bytes are already downloaded reads them from disk`() =
        runTest {
            val scope = TestScope(testScheduler)
            val uri = mediaUrl()
            downloads.store(uri, WAV)
            playback.plans[FILE_ID] = planAt(uri)
            val player = player(scope) { null }

            player.play(REQUEST)
            scope.runCurrent()
            TestPlayerRunHelper.run(exoPlayer).untilState(Player.STATE_READY)

            assertEquals("the core's plan was used", listOf(FILE_ID), playback.planRequests.map { it.second })
            assertEquals("every byte came from the cache", 0, server.requestCount)
            player.stop()
        }

    @Test
    fun `streaming a file that is not downloaded fetches it and caches nothing`() =
        runTest {
            val scope = TestScope(testScheduler)
            isUp = true
            val uri = mediaUrl()
            playback.plans[FILE_ID] = planAt(uri)
            val player = player(scope) { null }

            player.play(REQUEST)
            scope.runCurrent()
            TestPlayerRunHelper.run(exoPlayer).untilState(Player.STATE_READY)

            assertTrue("the file came from the server", server.requestCount > 0)
            assertTrue("the download cache holds only downloads", downloads.cache.keys.isEmpty())
            player.stop()
        }

    /** The core's plan for [FILE_ID], streamed from [uri] under a session. */
    private fun planAt(uri: String) = Fixtures.plan(listOf(Fixtures.part(FILE_ID).copy(url = uri)))

    private fun mediaUrl(): String = server.url("/v1/files/$FILE_ID/stream").toString()

    private companion object {
        const val FILE_ID = "file-1"
        val REQUEST =
            PlaybackRequest(
                mediaId = "movie-1",
                fileId = FILE_ID,
                title = "Le Samourai",
                durationSecs = 1.0,
            )

        /** One second of 16-bit mono silence at 8 kHz, as a WAV file. */
        val WAV: ByteArray =
            run {
                val sampleRate = 8_000
                val data = sampleRate * 2
                ByteBuffer
                    .allocate(44 + data)
                    .order(ByteOrder.LITTLE_ENDIAN)
                    .put("RIFF".toByteArray())
                    .putInt(36 + data)
                    .put("WAVE".toByteArray())
                    .put("fmt ".toByteArray())
                    .putInt(16)
                    .putShort(1)
                    .putShort(1)
                    .putInt(sampleRate)
                    .putInt(sampleRate * 2)
                    .putShort(2)
                    .putShort(16)
                    .put("data".toByteArray())
                    .putInt(data)
                    .array()
            }
    }
}
