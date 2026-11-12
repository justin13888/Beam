// Media3's `Tracks.Group` and `TrackGroup` constructors are @UnstableApi; a
// test builds the tracks a file would report by hand.
@file:UnstableApi

package dev.beam.android.core.media.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.TrackGroup
import androidx.media3.common.Tracks
import androidx.media3.common.util.UnstableApi
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.beam_client_core.StartSubtitle
import uniffi.beam_client_core.StartTracks

/**
 * Which of Media3's track groups the server meant (issue #329): the tracks
 * are built as a Matroska file with two audio streams, embedded subtitles,
 * captions in the video stream and a subtitle file beside it reports them.
 *
 * Robolectric because Media3's `Format` normalises a language code through
 * `Util`, which reads the platform's build constants as it loads.
 */
@RunWith(RobolectricTestRunner::class)
class StartTracksTest {
    private val english = audio("1", "eng")
    private val french = audio("2", "fre")
    private val unsupportedFrench = audio("2", "fre", supported = false)
    private val captions = text("cc", MimeTypes.APPLICATION_CEA608)
    private val embeddedEnglish = text("3", MimeTypes.APPLICATION_SUBRIP)
    private val embeddedSigns = text("4", MimeTypes.APPLICATION_SUBRIP)
    private val sidecar = text("1:sidecar:s1", MimeTypes.APPLICATION_SUBRIP)

    private val tracks = Tracks(listOf(english, french, captions, embeddedEnglish, embeddedSigns, sidecar))

    @Test
    fun `the named audio track is the file's audio stream at that position`() {
        val choice = tracks.startAudio(StartTracks(audioIndex = 1u, subtitle = null))

        assertEquals(StartChoice.Select(french, 0), choice)
    }

    @Test
    fun `an audio track that is not named, not there, or not decodable keeps the player's choice`() {
        assertSame(StartChoice.Keep, tracks.startAudio(StartTracks(audioIndex = null, subtitle = null)))
        assertSame(StartChoice.Keep, tracks.startAudio(StartTracks(audioIndex = 5u, subtitle = null)))
        assertSame(
            StartChoice.Keep,
            Tracks(listOf(english, unsupportedFrench)).startAudio(StartTracks(audioIndex = 1u, subtitle = null)),
        )
    }

    @Test
    fun `no named subtitle starts without subtitles`() {
        assertSame(StartChoice.Off, tracks.startSubtitle(StartTracks(audioIndex = 0u, subtitle = null)))
    }

    @Test
    fun `an embedded subtitle is counted among the file's own streams, not captions or sidecars`() {
        val choice = tracks.startSubtitle(StartTracks(null, StartSubtitle.Embedded(position = 1u)))

        assertEquals(StartChoice.Select(embeddedSigns, 0), choice)
    }

    @Test
    fun `a sidecar is found by the identity it was loaded under`() {
        val choice = tracks.startSubtitle(StartTracks(null, StartSubtitle.Sidecar(sidecarId = "s1")))

        assertEquals(StartChoice.Select(sidecar, 0), choice)
    }

    @Test
    fun `a subtitle that is not there keeps the player's choice`() {
        assertSame(StartChoice.Keep, tracks.startSubtitle(StartTracks(null, StartSubtitle.Embedded(position = 2u))))
        assertSame(StartChoice.Keep, tracks.startSubtitle(StartTracks(null, StartSubtitle.Sidecar(sidecarId = "s9"))))
    }

    private fun audio(
        id: String,
        language: String,
        supported: Boolean = true,
    ): Tracks.Group =
        group(
            Format
                .Builder()
                .setId(id)
                .setSampleMimeType(MimeTypes.AUDIO_AAC)
                .setLanguage(language),
            supported,
        )

    private fun text(
        id: String,
        mimeType: String,
    ): Tracks.Group = group(Format.Builder().setId(id).setSampleMimeType(mimeType), supported = true)

    private fun group(
        format: Format.Builder,
        supported: Boolean,
    ): Tracks.Group =
        Tracks.Group(
            TrackGroup(format.build()),
            false,
            intArrayOf(if (supported) C.FORMAT_HANDLED else C.FORMAT_UNSUPPORTED_SUBTYPE),
            booleanArrayOf(false),
        )
}
