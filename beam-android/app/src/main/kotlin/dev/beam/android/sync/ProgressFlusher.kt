package dev.beam.android.sync

import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.filter
import kotlinx.coroutines.flow.filterNotNull
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
 * sends the queue only when asked. This asks on two triggers:
 *
 * - **the active server becomes signed in**, which covers the start (the
 *   restore publishes the list, so this waits for it), a switch of server
 *   (the queue is per server, and only the active one's is sent), and a
 *   sign-in after the server refused the session;
 * - **a network comes back** after there was none.
 *
 * and then keeps asking while anything is left. The core stops a drain at the
 * first failed send and backs that entry off, so a drain that runs while the
 * server is down leaves the queue -- and a server that recovers while the
 * device stays online fires neither trigger. So after a drain that leaves
 * entries queued for a signed-in server, this drains again on the core's own
 * backoff schedule ([retryDelay]) until the queue is empty, the session is
 * refused, or another trigger starts over.
 *
 * A trigger arriving while a drain is running waits for that drain to finish
 * and then starts one more (with the retry schedule reset), so the queue is
 * never sent twice over at once; one arriving during a retry wait ends the
 * wait.
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

            // `collectLatest` cancels a running drain for a newer trigger, but
            // a send is never cut off: only the wait between retries is
            // cancellable, and the new drain starts once the send returns.
            merge(signedIn, reconnected).collectLatest { drainUntilEmpty() }
        }

        private suspend fun drainUntilEmpty() {
            var retries = 0
            while (withContext(NonCancellable) { flush() }) {
                retries += 1
                delay(retryDelay(retries))
            }
        }

        /** Drain once, and answer whether anything is left worth retrying. */
        private suspend fun flush(): Boolean {
            // Read now rather than carried from the trigger: a reconnect is
            // only worth a drain while the server it would go to accepts us.
            return try {
                val active = servers.activeServer() ?: return false
                if (active.state !is SessionState.Authenticated) return false
                playback.flushProgress()
                playback.pendingProgressCount() > 0u
            } catch (_: BeamException) {
                // Whatever was not sent stays queued, and is retried on the
                // same schedule. A refused session is already re-published
                // by the repository, which is how the shell learns of it,
                // and the state check above ends the retries on the next pass.
                true
            }
        }

        private companion object {
            /**
             * The wait before the [retry]th re-drain: 2 s, 4 s, 8 s, ... capped
             * at 64 s -- the schedule `ProgressQueue::record_failure` in
             * beam-client-core backs a failed entry off by, so a retry lands as
             * the entry falls due rather than before it (when the core would
             * send nothing). A server's `Retry-After` can push an entry later;
             * a retry before then is a local read that sends nothing.
             */
            fun retryDelay(retry: Int): Duration = (1L shl retry.coerceIn(1, MAX_BACKOFF_EXPONENT)).seconds

            private const val MAX_BACKOFF_EXPONENT = 6
        }
    }
