package dev.beam.android.sync

import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.beam_client_core.BeamException
import kotlin.time.Duration.Companion.hours
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds

/**
 * Positions recorded while offline reach the server: the queue is drained at
 * start, on a return from offline, and when another server becomes the
 * signed-in active one -- and never towards a server that would refuse it.
 */
class ProgressFlusherTest {
    private class Network(
        initial: Boolean,
    ) : NetworkAvailability {
        val state = MutableStateFlow(initial)
        override val isAvailable: Flow<Boolean> = state
    }

    @Test
    fun queued_progress_is_flushed_at_start_and_on_reconnect() =
        runTest {
            val servers =
                FakeServerRepository(
                    listOf(
                        Fixtures.server(id = "home", isActive = true),
                        Fixtures.server(id = "cabin", isActive = false),
                    ),
                    restored = true,
                )
            val playback = FakePlaybackRepository().apply { pending = 2u }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals("the start drains what an earlier run queued", 1, playback.flushCalls)
            assertEquals(0u, playback.pending)

            network.state.value = false
            playback.pending = 1u
            testScheduler.runCurrent()
            assertEquals("going offline is no moment to send", 1, playback.flushCalls)

            network.state.value = true
            testScheduler.runCurrent()
            assertEquals("coming back online drains what was queued offline", 2, playback.flushCalls)
            assertEquals(0u, playback.pending)

            servers.selectServer("cabin")
            testScheduler.runCurrent()
            assertEquals("the queue is per server, so a switch drains the new one", 3, playback.flushCalls)
        }

    @Test
    fun a_refused_session_holds_the_queue_until_sign_in_and_a_failed_drain_is_retried() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 1u
                    failWith = BeamException.Storage("the queue is locked")
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals(1, playback.flushCalls)
            assertEquals("a failed drain leaves the queue", 1u, playback.pending)

            servers.expire("home")
            playback.failWith = null
            network.state.value = false
            testScheduler.runCurrent()
            network.state.value = true
            testScheduler.runCurrent()
            assertEquals("a reconnect sends nothing towards a refused session", 1, playback.flushCalls)

            servers.completeLogin("home", "cookie")
            testScheduler.runCurrent()
            assertEquals("signing in again drains it, and the owner outlived the failure", 2, playback.flushCalls)
            assertEquals(0u, playback.pending)
        }

    @Test
    fun a_drain_that_throws_is_retried_on_the_backoff_with_no_trigger() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 1u
                    failWith = BeamException.Storage("the queue is locked")
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals(1, playback.flushCalls)

            testScheduler.advanceTimeBy(1.seconds)
            testScheduler.runCurrent()
            assertEquals("no retry before the first backoff step", 1, playback.flushCalls)

            playback.failWith = null
            testScheduler.advanceTimeBy(1.seconds)
            testScheduler.runCurrent()
            assertEquals("a thrown drain is retried 2 s later with no network change", 2, playback.flushCalls)
            assertEquals(0u, playback.pending)

            testScheduler.advanceTimeBy(1.hours)
            testScheduler.runCurrent()
            assertEquals("the drained queue is not asked again", 2, playback.flushCalls)
        }

    @Test
    fun a_server_that_recovers_while_the_device_stays_online_gets_the_queue_on_the_backoff() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 2u
                    serverDown = true
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals(1, playback.flushCalls)
            assertEquals("a down server takes nothing", 2u, playback.pending)

            testScheduler.advanceTimeBy(1.seconds)
            testScheduler.runCurrent()
            assertEquals("no retry before the first entry falls due", 1, playback.flushCalls)

            testScheduler.advanceTimeBy(1.seconds)
            testScheduler.runCurrent()
            assertEquals("the retry follows the core's backoff", 2, playback.flushCalls)

            playback.serverDown = false
            testScheduler.advanceTimeBy(4.seconds)
            testScheduler.runCurrent()
            assertEquals("the recovered server gets the queue with no network change", 3, playback.flushCalls)
            assertEquals(0u, playback.pending)

            testScheduler.advanceTimeBy(1.hours)
            testScheduler.runCurrent()
            assertEquals("an empty queue is not asked again", 3, playback.flushCalls)
        }

    @Test
    fun retries_stop_at_a_refused_session_and_a_new_trigger_starts_the_schedule_over() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 1u
                    serverDown = true
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            testScheduler.advanceTimeBy(2.seconds + 4.seconds + 1.milliseconds)
            testScheduler.runCurrent()
            assertEquals("two retries, 2 s then 4 s apart", 3, playback.flushCalls)

            network.state.value = false
            testScheduler.runCurrent()
            network.state.value = true
            testScheduler.runCurrent()
            assertEquals("a reconnect drains at once rather than at the next retry", 4, playback.flushCalls)
            testScheduler.advanceTimeBy(2.seconds)
            testScheduler.runCurrent()
            assertEquals("and the backoff starts again from its first step", 5, playback.flushCalls)

            servers.expire("home")
            testScheduler.advanceTimeBy(1.hours)
            testScheduler.runCurrent()
            assertEquals("nothing is retried towards a refused session", 5, playback.flushCalls)
            assertEquals(1u, playback.pending)
        }
}
