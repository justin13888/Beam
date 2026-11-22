package dev.beam.android.core.testing

import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.UserSummary

/** Where an awaited device login ends, for [FakeServerRepository.deviceLogin]. */
public sealed interface DeviceLoginEnding {
    /** The viewer approved: signed in as [user]. */
    public data class Approved(
        public val user: UserSummary,
    ) : DeviceLoginEnding

    /** The flow ended with [failure]: a refusal, an expiry, an unknown flow. */
    public data class Fails(
        public val failure: BeamException,
    ) : DeviceLoginEnding

    /** Nobody answers: the wait lasts until it is cancelled. */
    public data object Unanswered : DeviceLoginEnding
}
