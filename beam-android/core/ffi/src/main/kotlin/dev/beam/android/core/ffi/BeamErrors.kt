package dev.beam.android.core.ffi

import uniffi.beam_client_core.BeamException

/**
 * A failure, already phrased for a person.
 *
 * @property message what to show.
 * @property retryable whether offering a retry would be honest.
 * @property requiresSignIn whether the right response is to send the user to
 *   sign in rather than to show an error at all.
 */
public data class BeamFailure(
    val message: String,
    val retryable: Boolean,
    val requiresSignIn: Boolean = false,
)

/**
 * Turn a core error into something a screen can render.
 *
 * The *message* is decided here, because it is UI copy. Whether a retry is
 * honest is decided by the core, through `isRetryable`: `Network` and `Server`
 * carry the verdict on the error itself, and the rest are the core's table.
 * This file used to keep its own copy of that table, and offered a retry for
 * every `Server` status -- including a 415 or a 422, which three operations
 * declare, where resending the same refused body fails identically forever.
 */
public fun BeamException.toFailure(): BeamFailure =
    BeamFailure(
        message = sentence(),
        retryable = isRetryable(),
        requiresSignIn = this is BeamException.Unauthenticated || this is BeamException.SessionExpired,
    )

/**
 * What to tell the viewer.
 *
 * Deliberately exhaustive over [BeamException] rather than falling back to
 * `toString()`: an error surfaced to a user should read as a sentence about
 * their situation, not as a debug representation of ours.
 */
private fun BeamException.sentence(): String =
    when (this) {
        is BeamException.NoActiveServer -> "No server selected. Add a Beam server to get started."

        is BeamException.UnknownServer -> "That server is no longer set up on this device."

        is BeamException.InvalidServerUrl -> "That address does not look like a Beam server."

        is BeamException.Unauthenticated -> "Sign in to continue."

        is BeamException.SessionExpired -> "Your session expired. Sign in again to continue."

        is BeamException.Forbidden -> "You do not have permission to do that."

        // These two already carry the server's own explanation, which is more
        // specific than anything this layer could substitute for it.
        is BeamException.NotFound -> detail

        is BeamException.BadRequest -> detail

        is BeamException.RateLimited -> "The server is busy. Try again in $retryAfterSecs seconds."

        is BeamException.Server -> "The server had a problem handling that."

        is BeamException.Network -> "Could not reach the server. Check your connection."

        is BeamException.UntrustedCertificate -> "This server's security certificate is not trusted."

        is BeamException.Protocol -> "The server replied in a way this app did not understand."

        is BeamException.Storage -> "This device could not save that."
    }
