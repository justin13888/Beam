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
 * Deliberately exhaustive over [BeamException] rather than falling back to
 * `toString()`: an error surfaced to a user should read as a sentence about
 * their situation, not as a debug representation of ours.
 */
public fun BeamException.toFailure(): BeamFailure =
    when (this) {
        is BeamException.NoActiveServer -> {
            BeamFailure(
                message = "No server selected. Add a Beam server to get started.",
                retryable = false,
            )
        }

        is BeamException.UnknownServer -> {
            BeamFailure(
                message = "That server is no longer set up on this device.",
                retryable = false,
            )
        }

        is BeamException.InvalidServerUrl -> {
            BeamFailure(
                message = "That address does not look like a Beam server.",
                retryable = false,
            )
        }

        is BeamException.Unauthenticated -> {
            BeamFailure(
                message = "Sign in to continue.",
                retryable = false,
                requiresSignIn = true,
            )
        }

        is BeamException.SessionExpired -> {
            BeamFailure(
                message = "Your session expired. Sign in again to continue.",
                retryable = false,
                requiresSignIn = true,
            )
        }

        is BeamException.Forbidden -> {
            BeamFailure(
                message = "You do not have permission to do that.",
                retryable = false,
            )
        }

        // These two already carry the server's own explanation, which is more
        // specific than anything this layer could substitute for it.
        is BeamException.NotFound -> {
            BeamFailure(message = detail, retryable = false)
        }

        is BeamException.BadRequest -> {
            BeamFailure(message = detail, retryable = false)
        }

        is BeamException.RateLimited -> {
            BeamFailure(
                message = "The server is busy. Try again in $retryAfterSecs seconds.",
                retryable = true,
            )
        }

        is BeamException.Server -> {
            BeamFailure(
                message = "The server had a problem handling that.",
                retryable = true,
            )
        }

        // The core already decided whether this particular failure is worth
        // retrying, so a retry button appears only when one could work.
        is BeamException.Network -> {
            BeamFailure(
                message = "Could not reach the server. Check your connection.",
                retryable = retryable,
            )
        }

        is BeamException.UntrustedCertificate -> {
            BeamFailure(
                message = "This server's security certificate is not trusted.",
                retryable = false,
            )
        }

        is BeamException.Protocol -> {
            BeamFailure(
                message = "The server replied in a way this app did not understand.",
                retryable = false,
            )
        }

        is BeamException.Storage -> {
            BeamFailure(
                message = "This device could not save that.",
                retryable = true,
            )
        }
    }
