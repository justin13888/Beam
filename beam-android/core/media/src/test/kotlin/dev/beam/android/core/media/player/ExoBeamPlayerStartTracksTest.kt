// Media3's test players and hand-built tracks are @UnstableApi, like the
// ExoPlayer surface itself.
@file:UnstableApi

package dev.beam.android.core.media.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.TrackGroup
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.test.utils.TestExoPlayerBuilder
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakeTelemetryRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import uniffi.beam_client_core.RebufferMonitorInterface
import uniffi.beam_client_core.StartSubtitle
import uniffi.beam_client_core.StartTracks

/**
 * The player starts each part on the tracks the server named for the viewer
 * (issue #329), and the viewer's own pick wins over them. The tracks a file
 * would report are handed to the player's listener directly: Robolectric
 * hosts ExoPlayer, but loads no media to report any.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(RobolectricTestRunner::class)
class ExoBeamPlayerStartTracksTest {
    private val exoPlayer: ExoPlayer = TestExoPlayerBuilder(RuntimeEnvironment.getApplication()).build()
    private val playback = FakePlaybackRepository()

    private val english = group("1", MimeTypes.AUDIO_AAC, "eng")
    private val french = group("2", MimeTypes.AUDIO_AAC, "fre")
    private val signs = group("3", MimeTypes.APPLICATION_SUBRIP, "eng")
    private val tracks = Tracks(listOf(english, french, signs))

    @After
    fun tearDown() {
        exoPlayer.release()
    }

    private fun player(scope: TestScope) =
        ExoBeamPlayer(
            context = RuntimeEnvironment.getApplication(),
            exoPlayer = exoPlayer,
            repository = playback,
            telemetry = FakeTelemetryRepository(),
            clients = BeamHttpClientFactory(BeamHttpClientFactory.shared()),
            scope = scope,
            rebuffers = NeverSustained,
        )

    @Test
    fun `a part starts on the audio and subtitle tracks the server names`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[FILE_ID] =
                Fixtures.plan(
                    parts =
                        listOf(
                            Fixtures.part(FILE_ID, startTracks = StartTracks(1u, StartSubtitle.Embedded(0u))),
                        ),
                )
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()

            player.onTracksChanged(tracks)

            val parameters = exoPlayer.trackSelectionParameters
            assertEquals(listOf(0), parameters.overrides[french.mediaTrackGroup]?.trackIndices)
            assertEquals(listOf(0), parameters.overrides[signs.mediaTrackGroup]?.trackIndices)
            assertFalse(C.TRACK_TYPE_TEXT in parameters.disabledTrackTypes)
            player.stop()
        }

    @Test
    fun `the viewer's pick wins over what the server names for the next part`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[FILE_ID] =
                Fixtures.plan(
                    parts =
                        listOf(
                            Fixtures.part(FILE_ID, startTracks = StartTracks(1u, StartSubtitle.Embedded(0u))),
                            Fixtures.part("part-2", startTracks = StartTracks(0u, null)),
                        ),
                )
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()
            player.onTracksChanged(tracks)

            // The viewer keeps French audio by picking it; nothing about
            // subtitles. The second part names English and no subtitles.
            player.selectAudioTrack("2")
            exoPlayer.seekTo(1, 0L)
            player.onTracksChanged(tracks)

            val parameters = exoPlayer.trackSelectionParameters
            assertEquals(null, parameters.overrides[english.mediaTrackGroup])
            assertEquals(listOf(0), parameters.overrides[french.mediaTrackGroup]?.trackIndices)
            assertTrue("the second part starts without subtitles", C.TRACK_TYPE_TEXT in parameters.disabledTrackTypes)
            player.stop()
        }

    @Test
    fun `a part planned without the server's list keeps the player's own choice`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[FILE_ID] = Fixtures.plan(parts = listOf(Fixtures.part(FILE_ID, startTracks = null)))
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()

            player.onTracksChanged(tracks)

            assertTrue(exoPlayer.trackSelectionParameters.overrides.isEmpty())
            assertFalse(C.TRACK_TYPE_TEXT in exoPlayer.trackSelectionParameters.disabledTrackTypes)
            player.stop()
        }

    @Test
    fun `a new title starts free of the last title's picks and languages`() =
        runTest {
            val scope = TestScope(testScheduler)
            playback.plans[FILE_ID] =
                Fixtures.plan(
                    parts =
                        listOf(
                            Fixtures.part(FILE_ID, startTracks = StartTracks(1u, StartSubtitle.Embedded(0u))),
                        ),
                )
            playback.plans[NEXT_FILE_ID] =
                Fixtures.plan(parts = listOf(Fixtures.part(NEXT_FILE_ID, startTracks = null)))
            val player = player(scope)
            player.play(REQUEST)
            scope.runCurrent()
            player.onTracksChanged(tracks)
            // The viewer turns subtitles off. A pick of French audio would also
            // leave French preferred; the test player reports no current
            // tracks for `selectAudioTrack` to find, so that is set directly.
            player.selectSubtitleTrack(null)
            exoPlayer.trackSelectionParameters =
                exoPlayer.trackSelectionParameters
                    .buildUpon()
                    .setPreferredAudioLanguage("fre")
                    .setPreferredTextLanguage("eng")
                    .build()

            // The next title, played offline, carries no start tracks.
            player.play(REQUEST.copy(mediaId = "movie-2", fileId = NEXT_FILE_ID))
            scope.runCurrent()
            player.onTracksChanged(tracks)

            val parameters = exoPlayer.trackSelectionParameters
            assertTrue(parameters.overrides.isEmpty())
            assertTrue(parameters.preferredAudioLanguages.isEmpty())
            assertTrue(parameters.preferredTextLanguages.isEmpty())
            assertFalse(C.TRACK_TYPE_TEXT in parameters.disabledTrackTypes)
            player.stop()
        }

    private fun group(
        id: String,
        mimeType: String,
        language: String,
    ): Tracks.Group =
        Tracks.Group(
            TrackGroup(
                Format
                    .Builder()
                    .setId(id)
                    .setSampleMimeType(mimeType)
                    .setLanguage(language)
                    .build(),
            ),
            false,
            intArrayOf(C.FORMAT_HANDLED),
            booleanArrayOf(false),
        )

    /** A monitor that never judges rebuffering sustained; the rule is the core's, tested there. */
    private object NeverSustained : RebufferMonitorInterface {
        override fun stallStarted(nowMs: ULong): Boolean = false

        override fun stallEnded(nowMs: ULong): Boolean = false

        override fun poll(nowMs: ULong): Boolean = false

        override fun reset() = Unit
    }

    private companion object {
        const val FILE_ID = "file-1"
        const val NEXT_FILE_ID = "file-2"
        val REQUEST =
            PlaybackRequest(
                mediaId = "movie-1",
                fileId = FILE_ID,
                title = "Le Samourai",
                durationSecs = 3_000.0,
            )
    }
}
