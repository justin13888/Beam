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
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)))
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
}
