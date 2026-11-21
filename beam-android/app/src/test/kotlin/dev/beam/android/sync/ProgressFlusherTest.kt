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
import uniffi.beam_client_core.SessionState
import kotlin.time.Duration.Companion.hours
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.Duration.Companion.seconds

/**
 * Positions recorded while offline reach the server: the queue is drained at
 * start, on a return from offline, and when another server becomes signed in
 * -- and retried on the wait the core names, never on a schedule of this
 * side's own.
 */
class ProgressFlusherTest {
    private class Network(
        initial: Boolean,
    ) : NetworkAvailability {
        val state = MutableStateFlow(initial)
        override val isAvailable: Flow<Boolean> = state
    }

    @Test
    fun queued_progress_is_drained_at_start_on_reconnect_and_on_a_new_sign_in() =
        runTest {
            val servers =
                FakeServerRepository(
                    listOf(
                        Fixtures.server(id = "home", isActive = true),
                        Fixtures.server(id = "cabin", isActive = false, state = SessionState.LoggedOut),
                    ),
                    restored = true,
                )
            val playback = FakePlaybackRepository().apply { pending = 2u }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals("the start drains what an earlier run queued", 1, playback.drainCalls)
            assertEquals(0u, playback.pending)

            network.state.value = false
            playback.pending = 1u
            testScheduler.runCurrent()
            assertEquals("going offline is no moment to send", 1, playback.drainCalls)

            network.state.value = true
            testScheduler.runCurrent()
            assertEquals("coming back online drains what was queued offline", 2, playback.drainCalls)
            assertEquals(0u, playback.pending)

            servers.selectServer("cabin")
            testScheduler.runCurrent()
            assertEquals(
                "the core drains every server whichever is active, so a switch alone is no trigger",
                2,
                playback.drainCalls,
            )

            servers.completeLogin("cabin", "cookie")
            testScheduler.runCurrent()
            assertEquals("a server that signs in gets what was queued for it", 3, playback.drainCalls)
        }

    @Test
    fun the_retry_waits_exactly_as_long_as_the_core_names_and_stops_when_it_names_none() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 2u
                    serverDown = true
                    retryInSecs = 90uL
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals(1, playback.drainCalls)
            assertEquals("a down server takes nothing", 2u, playback.pending)

            testScheduler.advanceTimeBy(90.seconds - 1.milliseconds)
            testScheduler.runCurrent()
            assertEquals("no retry before the wait the core named", 1, playback.drainCalls)

            playback.serverDown = false
            testScheduler.advanceTimeBy(1.milliseconds)
            testScheduler.runCurrent()
            assertEquals("the recovered server gets the queue with no network change", 2, playback.drainCalls)
            assertEquals(0u, playback.pending)

            testScheduler.advanceTimeBy(1.hours)
            testScheduler.runCurrent()
            assertEquals("a drain that names no wait is not asked again", 2, playback.drainCalls)
        }

    @Test
    fun a_drain_that_throws_is_retried_on_the_cores_schedule_with_no_trigger() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 1u
                    failWith = BeamException.Storage("the queue is locked")
                    failedDrainRetrySecs = 5uL
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            testScheduler.advanceTimeBy(5.seconds + 1.milliseconds)
            testScheduler.runCurrent()
            assertEquals("a thrown drain is retried after the core's wait", 2, playback.drainCalls)
            assertEquals(
                "and each failure in a row asks for the next step",
                listOf(1u, 2u),
                playback.failedDrainAttempts,
            )

            playback.failWith = null
            testScheduler.advanceTimeBy(5.seconds)
            testScheduler.runCurrent()
            assertEquals(3, playback.drainCalls)
            assertEquals(0u, playback.pending)

            testScheduler.advanceTimeBy(1.hours)
            testScheduler.runCurrent()
            assertEquals("the drained queue is not asked again", 3, playback.drainCalls)
        }

    @Test
    fun a_new_trigger_ends_the_wait_and_a_success_resets_the_failure_count() =
        runTest {
            val servers = FakeServerRepository(listOf(Fixtures.server(id = "home", isActive = true)), restored = true)
            val playback =
                FakePlaybackRepository().apply {
                    pending = 1u
                    failWith = BeamException.Storage("the queue is locked")
                    failedDrainRetrySecs = 60uL
                }
            val network = Network(initial = true)
            backgroundScope.launch { ProgressFlusher(servers, playback, network).run() }

            testScheduler.runCurrent()
            assertEquals(1, playback.drainCalls)

            network.state.value = false
            testScheduler.runCurrent()
            network.state.value = true
            testScheduler.runCurrent()
            assertEquals("a reconnect drains at once rather than at the next retry", 2, playback.drainCalls)
            assertEquals("a new trigger starts the count over", listOf(1u, 1u), playback.failedDrainAttempts)

            playback.failWith = null
            playback.serverDown = true
            playback.retryInSecs = 3uL
            testScheduler.advanceTimeBy(60.seconds)
            testScheduler.runCurrent()
            playback.failWith = BeamException.Storage("locked again")
            testScheduler.advanceTimeBy(3.seconds)
            testScheduler.runCurrent()
            assertEquals(4, playback.drainCalls)
            assertEquals("a drain that answered resets the count", listOf(1u, 1u, 1u), playback.failedDrainAttempts)
        }
}
