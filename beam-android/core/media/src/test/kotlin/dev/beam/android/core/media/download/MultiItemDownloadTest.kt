// Media3 marks its offline surface @UnstableApi; see BeamDownloadManager.kt.
@file:UnstableApi

package dev.beam.android.core.media.download

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.Download
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.beam_client_core.DownloadState

/**
 * A download whose plan is more than one item -- a film across two clips,
 * the second with a subtitle file beside it -- through the harness's real
 * manager, core and server. Each item is its own Media3 download, and every
 * command on the download reaches each of them.
 */
@RunWith(RobolectricTestRunner::class)
class MultiItemDownloadTest : DownloadRepositoryHarness() {
    @Test
    fun `every part and subtitle file of a download is queued, paused and resumed`() =
        runTest {
            sources["/v1/media/$TWO_PART_MEDIA/sources"] = TWO_PART_SOURCES
            // Held paused, as with no network, so every item stays where the
            // commands put it rather than finishing first.
            repository.manager().pauseDownloads()
            observe()

            enqueue(CD2, on = HOME, mediaId = TWO_PART_MEDIA)
            awaitUntil { indexed().size == 3 }
            assertEquals(setOf(CD1, CD2, SIDECAR), indexed().map { it.request.id }.toSet())
            assertEquals(
                "the core's record names every item",
                listOf(CD1, CD2, SIDECAR),
                core.downloads().single().itemIds(),
            )

            repository.pause(CD2)
            awaitUntil { indexed().all { it.stopReason == BeamDownloadManager.STOP_REASON_USER } }
            assertTrue(indexed().all { it.state == Download.STATE_STOPPED })
            awaitUntil { stateOf(CD2) == DownloadState.PAUSED }

            repository.resume(CD2)
            awaitUntil { indexed().all { it.stopReason == Download.STOP_REASON_NONE } }
            assertTrue(indexed().all { it.state == Download.STATE_QUEUED })
            awaitUntil { stateOf(CD2) == DownloadState.QUEUED }
        }

    private companion object {
        const val TWO_PART_MEDIA = "a0a0a0a0-0000-4000-8000-000000000001"
        const val CD1 = "c1c1c1c1-0000-4000-8000-000000000001"
        const val CD2 = "c2c2c2c2-0000-4000-8000-000000000002"
        const val SIDECAR = "5d5d5d5d-0000-4000-8000-000000000002"

        /** The title's one source: two clips, the second with a sidecar subtitle file. */
        val TWO_PART_SOURCES =
            """
            {"items":[
              {"file_id":"$CD1","is_primary":true,"edition":null,"episode_span":null,
               "parts":[
                 {"file_id":"$CD1","part_number":1,"size_bytes":3000,"duration_secs":3000.0,
                  "subtitle_tracks":[],
                  "stream_url":"/v1/files/$CD1/stream","download_url":"/v1/files/$CD1/download"},
                 {"file_id":"$CD2","part_number":2,"size_bytes":1000,"duration_secs":2800.0,
                  "subtitle_tracks":[
                    {"origin":"sidecar","index":null,"sidecar_id":"$SIDECAR",
                     "codec":"subrip","language":"spa","title":null,
                     "is_default":false,"is_forced":true,"is_hearing_impaired":false,"is_text":true,
                     "url":"/v1/files/$CD2/subtitles/$SIDECAR","webvtt_url":null}],
                  "stream_url":"/v1/files/$CD2/stream","download_url":"/v1/files/$CD2/download"}],
               "size_bytes":4000,"mime_type":null,"container_format":"mpegts","duration_secs":5800.0,
               "video_tracks":[{"index":0,"codec":"h264","width":1920,"height":1080,
                                "frame_rate":null,"bit_rate":null,"hdr_format":null}],
               "audio_tracks":[],"subtitle_tracks":[],
               "stream_url":"/v1/files/$CD1/stream","download_url":"/v1/files/$CD1/download"}],
             "page_info":{"has_next_page":false,"has_previous_page":false,
                          "start_cursor":null,"end_cursor":null}}
            """.trimIndent()
    }
}
