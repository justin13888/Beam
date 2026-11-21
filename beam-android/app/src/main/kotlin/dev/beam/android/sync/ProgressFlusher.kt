package dev.beam.android.sync

import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.merge
import kotlinx.coroutines.withContext
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.SessionState
import javax.inject.Inject
import javax.inject.Singleton
import kotlin.time.Duration
import kotlin.time.Duration.Companion.seconds

/**
 * The one owner of draining the core's durable progress queue.
 *
 * A position the server could not take -- no network, a server down, a
 * session refused -- is queued by the core rather than lost, and the core
 * sends the queue only when asked. Each server's queue goes to that server
 * with its own session, whichever is active, so this asks on two triggers:
 *
 * - **the set of signed-in servers changes**, which covers the start (the
 *   restore publishes the list, so this waits for it) and a sign-in after a
 *   server refused the session;
 * - **a network comes back** after there was none.
 *
 * and then keeps asking while the core says something is left. The core stops
 * a server's drain at the first failed send and backs that entry off, so a
 * drain that runs while a server is down leaves its queue -- and a server that
 * recovers while the device stays online fires neither trigger. The core's
 * answer names when the next drain could send something (its back-off, or a
 * server's `Retry-After`), so this waits exactly that long and drains again,
 * until the core names no wait or another trigger starts over. The schedule
 * is the core's; this keeps none of its own.
 *
 * A trigger arriving while a drain is running waits for that drain to finish
 * and then starts one more, so the queue is never sent twice over at once;
 * one arriving during a retry wait ends the wait.
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
                            .filter { it.state is SessionState.Authenticated }
                            .map { it.id }
                            .toSet()
                    }.distinctUntilChanged()
                    .filter { it.isNotEmpty() }
                    .map { }
            // The first answer is the state at start, which the restore
            // already covers; only a return from offline is a trigger.
            val reconnected =
                network.isAvailable
                    .distinctUntilChanged()
                    .drop(1)
                    .filter { it }
                    .map { }

            // `collectLatest` cancels a running drain for a newer trigger, but
            // a send is never cut off: only the wait between retries is
            // cancellable, and the new drain starts once the send returns.
            merge(signedIn, reconnected).collectLatest { drainUntilEmpty() }
        }

        private suspend fun drainUntilEmpty() {
            var failures = 0u
            while (true) {
                val wait: Duration =
                    withContext(NonCancellable) {
                        try {
                            // The core leaves servers that are not signed in
                            // alone, and names no wait once nothing a drain
                            // could send is left.
                            val drained = playback.drainProgress()
                            failures = 0u
                            drained.retryInSecs?.toLong()?.seconds
                        } catch (_: BeamException) {
                            // A queue that could not be read backed nothing
                            // off, so the core's schedule is asked directly.
                            failures += 1u
                            playback.progressRetryDelaySecs(failures).toLong().seconds
                        }
                    } ?: return
                delay(wait)
            }
        }
    }
