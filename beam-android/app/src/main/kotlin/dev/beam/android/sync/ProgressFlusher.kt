package dev.beam.android.sync

import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import kotlinx.coroutines.flow.conflate
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.SessionState
import javax.inject.Inject
import javax.inject.Singleton

/**
 * The one owner of draining the core's durable progress queue.
 *
 * A position the server could not take -- no network, a server down, a
 * session refused -- is queued by the core rather than lost, and the core
 * sends the queue only when asked. This asks on two triggers:
 *
 * - **the active server becomes signed in**, which covers the start (the
 *   restore publishes the list, so this waits for it), a switch of server
 *   (the queue is per server, and only the active one's is sent), and a
 *   sign-in after the server refused the session;
 * - **a network comes back** after there was none.
 *
 * A server that was down and recovers while the device stays online fires
 * neither: what was queued for it waits for the next of these triggers
 * (at the latest the next start). The core also stops a drain at the first
 * failed send and backs each entry off, so a drain that runs while the
 * server is still down leaves the queue for a later trigger.
 *
 * Triggers arriving while a drain is running fold into one more drain after
 * it, so the queue is never sent twice over at once.
 */
@Singleton
internal class ProgressFlusher
    @Inject
    constructor(
        private val servers: ServerRepository,
        private val playback: PlaybackRepository,
        private val network: NetworkAvailability,
    ) {
        /** Drain on every trigger, until the caller's scope is cancelled. */
        suspend fun run() {
            val signedIn =
                servers.servers
                    .map { known ->
                        known
                            .firstOrNull { it.isActive }
                            ?.takeIf { it.state is SessionState.Authenticated }
                            ?.id
                    }.distinctUntilChanged()
                    .filterNotNull()
                    .map { }
            // The first answer is the state at start, which the restore
            // already covers; only a return from offline is a trigger.
            val reconnected =
                network.isAvailable
                    .distinctUntilChanged()
                    .drop(1)
                    .filter { it }
                    .map { }

            merge(signedIn, reconnected).conflate().collect { flush() }
        }

        private suspend fun flush() {
            // Read now rather than carried from the trigger: a reconnect is
            // only worth a drain while the server it would go to accepts us.
            try {
                val active = servers.activeServer() ?: return
                if (active.state !is SessionState.Authenticated) return
                playback.flushProgress()
            } catch (_: BeamException) {
                // Whatever was not sent stays queued for the next trigger. A
                // refused session is already re-published by the repository,
                // which is how the shell learns of it.
            }
        }
    }
