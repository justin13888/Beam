package dev.beam.android.core.media.player

import androidx.media3.common.C
import androidx.media3.common.MimeTypes
import dev.beam.android.core.testing.Fixtures
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

// Robolectric because Media3's `Util`, which normalises a language code, reads
// the platform's build constants as it loads.
@RunWith(RobolectricTestRunner::class)
class SidecarSubtitlesTest {
    @Test
    fun `a SubRip file is loaded as stored, which ExoPlayer parses itself`() {
        val sidecar = sidecarSubtitle(Fixtures.sidecar(codec = "subrip", webvtt = true))!!

        assertEquals(MimeTypes.APPLICATION_SUBRIP, sidecar.mimeType)
        assertEquals("https://beam.test/v1/files/file-1/subtitles/sidecar-1", sidecar.url)
        assertEquals("sidecar:sidecar-1", sidecar.id)
    }

    @Test
    fun `a format ExoPlayer cannot parse falls back to the server's WebVTT`() {
        val sidecar = sidecarSubtitle(Fixtures.sidecar(codec = "mov_text", webvtt = true))!!

        assertEquals(MimeTypes.TEXT_VTT, sidecar.mimeType)
        assertTrue(sidecar.url.endsWith("/webvtt"))
    }

    @Test
    fun `a sidecar with nothing ExoPlayer can render is left out`() {
        assertNull(sidecarSubtitle(Fixtures.sidecar(codec = "hdmv_pgs_subtitle", webvtt = false)))
    }

    @Test
    fun `forced and SDH tracks carry the flags the selector reads`() {
        val forced = sidecarSubtitle(Fixtures.sidecar(isForced = true))!!
        val sdh = sidecarSubtitle(Fixtures.sidecar(isHearingImpaired = true))!!

        assertEquals(C.SELECTION_FLAG_FORCED, forced.selectionFlags and C.SELECTION_FLAG_FORCED)
        assertEquals(0, forced.roleFlags)
        assertEquals(C.ROLE_FLAG_DESCRIBES_MUSIC_AND_SOUND, sdh.roleFlags)
        assertEquals(0, sdh.selectionFlags)
    }

    @Test
    fun `a track's own title names it, with forced and SDH said aloud`() {
        assertEquals("Commentary", subtitleLabel(Fixtures.sidecar(title = "Commentary")))
        assertEquals(
            "Signs (Forced, SDH)",
            subtitleLabel(Fixtures.sidecar(title = "Signs", isForced = true, isHearingImpaired = true)),
        )
        assertEquals("SDH", subtitleLabel(Fixtures.sidecar(language = null, isHearingImpaired = true)))
        assertNull(subtitleLabel(Fixtures.sidecar(language = null)))
    }

    @Test
    fun `a language code is named rather than shown as a code`() {
        // Whatever the device's language, the three-letter code a file carries
        // is not what a viewer should read.
        val named = languageName("eng")
        assertTrue(named != null && !named.equals("eng", ignoreCase = true))
        assertEquals(named, languageName("en"))
        assertNull(languageName(" "))
    }
}
