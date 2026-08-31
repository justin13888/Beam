package dev.beam.android.core.media.player

import dev.beam.android.core.model.PlaybackRequest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

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
            PlaybackFailure(
                message = "This device cannot decode this file.",
                isRetryable = false,
                suggestsAnotherSource = true,
            )

        assertTrue(failure.suggestsAnotherSource)
        assertTrue(!failure.isRetryable)
    }
}
