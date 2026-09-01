package dev.beam.android.core.ffi

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.CertificateDetails

/**
 * RFC 9457's "the status code is the whole story", which is what the core
 * reports for a failure that carried no problem type of its own.
 */
private const val ABOUT_BLANK = "about:blank"

/**
 * Every message a viewer sees for a failure comes through here, so these
 * assert the two properties that decide what the UI does: whether a retry
 * button appears, and whether the right response is to send them to sign in
 * instead of showing an error at all.
 */
class BeamErrorsTest {
    @Test
    fun `a transient network failure offers a retry`() {
        val failure = BeamException.Network("connection reset", retryable = true).toFailure()

        assertTrue(failure.retryable)
        assertFalse(failure.requiresSignIn)
    }

    @Test
    fun `a permanent network failure does not offer a retry`() {
        // The core decides this, not the UI. A retry button that cannot work
        // is worse than none: it invites the viewer to keep pressing it.
        val failure = BeamException.Network("no route to host", retryable = false).toFailure()

        assertFalse(failure.retryable)
    }

    @Test
    fun `an expired session sends the viewer to sign in rather than to an error`() {
        val failure = BeamException.SessionExpired().toFailure()

        assertTrue(failure.requiresSignIn)
        assertFalse(
            "signing in again is the action, so a retry button would be a dead end",
            failure.retryable,
        )
    }

    @Test
    fun `being unauthenticated sends the viewer to sign in`() {
        assertTrue(BeamException.Unauthenticated().toFailure().requiresSignIn)
    }

    @Test
    fun `a forbidden action is not a sign-in prompt`() {
        // The distinction matters: signing in again cannot grant a permission
        // the account does not have, so offering it would send the viewer
        // round a loop.
        val failure = BeamException.Forbidden("admin only", ABOUT_BLANK).toFailure()

        assertFalse(failure.requiresSignIn)
        assertFalse(failure.retryable)
    }

    @Test
    fun `the server's own explanation is preserved where it is more specific`() {
        // "No title with that id" tells the viewer something; "Something went
        // wrong" does not.
        val detail = "No title with id 0f3c"
        assertEquals(detail, BeamException.NotFound(detail, ABOUT_BLANK).toFailure().message)
        assertEquals(detail, BeamException.BadRequest(detail, ABOUT_BLANK).toFailure().message)
    }

    @Test
    fun `a missing source file is not phrased as the viewer's mistake`() {
        // Both of these are 404s, so the status cannot separate them -- which
        // is the whole reason the core carries the problem type. One means the
        // viewer asked for something that is not there. The other means the
        // catalogue still lists the title and the server no longer has its
        // file, which nothing the viewer does will fix.
        val absent =
            BeamException
                .NotFound("No title with id 0f3c", "https://beam.example/reference/errors/#media-not-found")
                .toFailure()
        val diverged =
            BeamException
                .NotFound(
                    "Source video file not found",
                    "https://beam.example/reference/errors/#source-file-missing",
                ).toFailure()

        assertEquals("No title with id 0f3c", absent.message)
        assertTrue(
            "a viewer cannot act on a missing mount, so say who can: ${diverged.message}",
            diverged.message.contains("administrator"),
        )
        assertFalse("rescanning is not something a retry button does", diverged.retryable)
    }

    @Test
    fun `rate limiting says how long to wait`() {
        val failure = BeamException.RateLimited(30uL).toFailure()

        assertTrue(failure.message.contains("30"))
        assertTrue(failure.retryable)
    }

    @Test
    fun `a server fault carries its retryability rather than restating it`() {
        // `retryable` is decided in the core (`classify`) and travels on the
        // error, so this asserts that the mapping *reads* it -- both ways
        // round. It used to answer `true` for every Server status, which put a
        // retry button on a 415 that will be refused identically forever.
        assertTrue(BeamException.Server(500u, true, "boom", ABOUT_BLANK).toFailure().retryable)
        assertFalse(BeamException.Server(415u, false, "no", ABOUT_BLANK).toFailure().retryable)
        assertFalse(BeamException.Protocol("missing field").toFailure().retryable)
    }

    @Test
    fun `an untrusted certificate is never presented as retryable`() {
        // It is a decision, not a fault, and the trust prompt handles it.
        val failure = BeamException.UntrustedCertificate("beam.local", certificate()).toFailure()

        assertFalse(failure.retryable)
    }

    @Test
    fun `no message is empty or a debug representation`() {
        // A message reaching a screen must read as a sentence about the
        // viewer's situation, not as our type name.
        val every =
            listOf(
                BeamException.NoActiveServer(),
                BeamException.UnknownServer("s1"),
                BeamException.InvalidServerUrl("nope"),
                BeamException.Unauthenticated(),
                BeamException.SessionExpired(),
                BeamException.Forbidden("no", ABOUT_BLANK),
                BeamException.NotFound("gone", ABOUT_BLANK),
                BeamException.BadRequest("bad", ABOUT_BLANK),
                BeamException.RateLimited(5uL),
                BeamException.Server(503u, true, "down", ABOUT_BLANK),
                BeamException.Network("offline", true),
                BeamException.UntrustedCertificate("beam.local", certificate()),
                BeamException.Protocol("garbled"),
                BeamException.Storage("full"),
            )

        every.forEach { error ->
            val message = error.toFailure().message
            assertTrue("empty message for $error", message.isNotBlank())
            assertFalse(
                "message for $error leaks a type name: $message",
                message.contains("BeamException") || message.contains("uniffi"),
            )
        }
    }

    @Test
    fun `storage failures are retryable because they are usually transient`() {
        // A full disk or a locked keystore clears; the viewer retrying after
        // freeing space is a real path to success.
        assertTrue(BeamException.Storage("no space").toFailure().retryable)
    }

    private fun certificate() =
        CertificateDetails(
            sha256Fingerprint = "AA:BB",
            spkiSha256Base64 = "c3BraQ==",
            subject = "CN=beam.local",
            issuer = "CN=beam.local",
            notBeforeUnix = 0L,
            notAfterUnix = Long.MAX_VALUE,
            subjectAltNames = listOf("beam.local"),
            serialHex = "01",
            isSelfSigned = true,
            isExpired = false,
        )
}
