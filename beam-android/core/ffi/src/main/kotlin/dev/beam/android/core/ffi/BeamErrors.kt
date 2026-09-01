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
 * The one problem type this layer reads.
 *
 * Matched as a suffix rather than as a whole URI: the type is a fragment on
 * beam-server's published error reference, so the origin in front of it moves
 * with the deployment while the code after the `#` is the stable half.
 */
private const val SOURCE_FILE_MISSING = "#source-file-missing"

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
        //
        // The one exception is the 404 that is not about what the viewer
        // asked for. `source-file-missing` means the catalogue still lists
        // the file and the disk no longer has it, so the server's "Source
        // video file not found" would read to a viewer as though they had
        // asked for the wrong thing. Nothing they do fixes it; someone with
        // access to the server has to. Telling the two apart is what the
        // problem type is for -- the status cannot.
        is BeamException.NotFound -> {
            if (code.endsWith(SOURCE_FILE_MISSING)) {
                BeamFailure(
                    message =
                        "This title is in the library but its file is missing from the " +
                            "server. Ask an administrator to rescan the library.",
                    retryable = false,
                )
            } else {
                BeamFailure(message = detail, retryable = false)
            }
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
