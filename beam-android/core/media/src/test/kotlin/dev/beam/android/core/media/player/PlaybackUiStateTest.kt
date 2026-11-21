package dev.beam.android.core.media.player

import android.net.Uri
import androidx.media3.common.PlaybackException
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
import dev.beam.android.core.ffi.BeamFailure
import dev.beam.android.core.model.PlaybackRequest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

/** Robolectric for the [DataSpec] a refused response carries, which needs a real [Uri]. */
@RunWith(RobolectricTestRunner::class)
class PlaybackUiStateTest {
    @Test
    fun `progress is null while the duration is unknown`() {
        // An indeterminate scrubber is honest; one confidently rendering 0%
        // for a title of unknown length is not.
        val state = PlaybackUiState(positionMs = 5_000L)

        assertNull(state.progress)
    }

    @Test
    fun `progress is the fraction watched`() {
        val state = PlaybackUiState(positionMs = 30_000L, durationMs = 120_000L)

        assertEquals(0.25f, state.progress!!, 0.0001f)
    }

    @Test
    fun `progress never leaves the unit interval`() {
        // ExoPlayer can report a position slightly past the duration at the
        // end of an item; a progress bar overshooting its track is a visible
        // glitch.
        val past = PlaybackUiState(positionMs = 121_000L, durationMs = 120_000L)

        assertEquals(1f, past.progress!!, 0.0001f)
    }

    @Test
    fun `a fresh state is idle`() {
        assertTrue(PlaybackUiState().isIdle)
    }

    @Test
    fun `a state carrying a request is not idle`() {
        val state =
            PlaybackUiState(
                request =
                    PlaybackRequest(
                        mediaId = "m1",
                        fileId = "f1",
                        title = "Arrival",
                    ),
            )

        assertTrue(!state.isIdle)
    }

    @Test
    fun `a decoder failure offers another source instead of a retry`() {
        // Direct play means there is no server-side fallback (ADR-0004), so
        // retrying the same bytes would fail identically every time.
        val failure =
            PlaybackException("decoder", null, PlaybackException.ERROR_CODE_DECODING_FORMAT_UNSUPPORTED)
                .asPlaybackFailure()

        assertTrue(failure.suggestsAnotherSource)
        assertFalse(failure.isRetryable)
        assertFalse(failure.requiresSignIn)
    }

    @Test
    fun `a refused session mid-stream sends the viewer to sign in`() {
        val failure = httpFailure(401).asPlaybackFailure()

        assertTrue("a 401 is the session, not the file", failure.requiresSignIn)
        assertFalse("the same cookie is refused the same way again", failure.isRetryable)
        assertFalse(failure.suggestsAnotherSource)
        assertEquals("Your session expired. Sign in again to continue.", failure.message)
    }

    @Test
    fun `a file the viewer can no longer reach is access lost, not an expired session`() {
        for (status in listOf(403, 404)) {
            val failure = httpFailure(status).asPlaybackFailure()

            assertFalse("$status: the session was accepted", failure.requiresSignIn)
            assertFalse("$status: asking again is refused again", failure.isRetryable)
            assertEquals("This file is no longer available to you.", failure.message)
        }
    }

    @Test
    fun `any other refusal stays a retryable server failure`() {
        for (status in listOf(400, 416, 500, 503)) {
            val failure = httpFailure(status).asPlaybackFailure()

            assertFalse("$status", failure.requiresSignIn)
            assertTrue("$status", failure.isRetryable)
            assertEquals("The server refused to send this file.", failure.message)
        }
    }

    @Test
    fun `a status wrapped once more is still found`() {
        val wrapped =
            PlaybackException(
                "wrapped",
                java.io.IOException("loader", invalidResponse(401)),
                PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
            )

        assertTrue(wrapped.asPlaybackFailure().requiresSignIn)
    }

    @Test
    fun `a 401 cause under another error code is not read as a session refusal`() {
        // Only a bad-status failure is the server answering; the status is not
        // looked for under any other code.
        val failure =
            PlaybackException(
                "network",
                invalidResponse(401),
                PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
            ).asPlaybackFailure()

        assertFalse(failure.requiresSignIn)
        assertEquals("Lost the connection to the server.", failure.message)
    }

    @Test
    fun `a core failure that needs sign-in keeps saying so`() {
        val failure =
            BeamFailure("Your session expired. Sign in again to continue.", retryable = false, requiresSignIn = true)
                .asPlaybackFailure()

        assertTrue(failure.requiresSignIn)
        assertFalse(failure.isRetryable)
    }

    private fun httpFailure(status: Int): PlaybackException =
        PlaybackException("bad status", invalidResponse(status), PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS)

    private fun invalidResponse(status: Int): HttpDataSource.InvalidResponseCodeException =
        HttpDataSource.InvalidResponseCodeException(
            status,
            null,
            null,
            emptyMap(),
            DataSpec(Uri.parse("https://beam.test/v1/files/f1/stream")),
            ByteArray(0),
        )
}
