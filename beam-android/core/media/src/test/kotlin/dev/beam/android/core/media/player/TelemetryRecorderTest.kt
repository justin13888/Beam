package dev.beam.android.core.media.player

import dev.beam.android.core.testing.FakeTelemetryRepository
import dev.beam.android.core.testing.TelemetryEvent
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.beam_client_core.StartFailureCause
import uniffi.beam_client_core.SwitchTrigger

@OptIn(ExperimentalCoroutinesApi::class)
class TelemetryRecorderTest {
    private fun recorder(
        telemetry: FakeTelemetryRepository,
        scope: TestScope,
    ) = TelemetryRecorder(telemetry, scope)

    @Test
    fun `a file starts once however many frames follow`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val recorder = recorder(telemetry, TestScope(testScheduler))

            recorder.begin("file-1")
            recorder.firstFrame(audioTrackIndex = 1u)
            recorder.firstFrame(audioTrackIndex = 1u)

            assertEquals(listOf(TelemetryEvent.Start("file-1", 1u)), telemetry.allEvents)
        }

    @Test
    fun `a stall before the first frame is part of starting, not a rebuffer`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val recorder = recorder(telemetry, TestScope(testScheduler))

            recorder.begin("file-1")
            recorder.stalled(durationMs = 4_000L)
            recorder.firstFrame(audioTrackIndex = null)
            recorder.stalled(durationMs = 2_500L)

            assertEquals(
                listOf(
                    TelemetryEvent.Start("file-1", null),
                    TelemetryEvent.Rebuffer("file-1", 2_500uL),
                ),
                telemetry.allEvents,
            )
        }

    @Test
    fun `a failure before the first frame is a start failure and after it is nothing`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val recorder = recorder(telemetry, TestScope(testScheduler))

            recorder.begin("file-1")
            recorder.failed(isNetwork = true, audioTrackIndex = 0u)
            recorder.failed(isNetwork = false, audioTrackIndex = 0u)
            recorder.begin("file-2")
            recorder.firstFrame(audioTrackIndex = null)
            recorder.failed(isNetwork = false, audioTrackIndex = null)

            assertEquals(
                listOf(
                    TelemetryEvent.StartFailure("file-1", StartFailureCause.Network, 0u),
                    TelemetryEvent.Start("file-2", null),
                ),
                telemetry.allEvents,
            )
        }

    @Test
    fun `a player failure that is not the connection is counted as other`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val recorder = recorder(telemetry, TestScope(testScheduler))

            recorder.begin("file-1")
            recorder.failed(isNetwork = false, audioTrackIndex = null)

            assertEquals(
                listOf(TelemetryEvent.StartFailure("file-1", StartFailureCause.Other, null)),
                telemetry.allEvents,
            )
        }

    @Test
    fun `a new file is judged from scratch`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val recorder = recorder(telemetry, TestScope(testScheduler))

            recorder.begin("file-1")
            recorder.firstFrame(audioTrackIndex = null)
            recorder.switched(from = "file-1", to = "file-2", trigger = SwitchTrigger.AUTO)
            recorder.begin("file-2")
            recorder.stalled(durationMs = 3_000L)
            recorder.firstFrame(audioTrackIndex = null)

            assertEquals(
                listOf(
                    TelemetryEvent.Start("file-1", null),
                    TelemetryEvent.SourceSwitch("file-1", "file-2", SwitchTrigger.AUTO),
                    TelemetryEvent.Start("file-2", null),
                ),
                telemetry.allEvents,
            )
        }

    @Test
    fun `nothing is reported outside a file`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val scope = TestScope(testScheduler)
            val recorder = recorder(telemetry, scope)

            recorder.firstFrame(audioTrackIndex = null)
            recorder.begin("file-1")
            recorder.end()
            recorder.failed(isNetwork = true, audioTrackIndex = null)
            scope.runCurrent()

            assertTrue(telemetry.allEvents.isEmpty())
        }

    @Test
    fun `ending playback sends what is waiting`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val scope = TestScope(testScheduler)
            val recorder = recorder(telemetry, scope)
            recorder.begin("file-1")
            recorder.firstFrame(audioTrackIndex = null)

            recorder.end()
            scope.runCurrent()

            assertEquals(1, telemetry.flushCount)
            assertEquals(listOf(TelemetryEvent.Start("file-1", null)), telemetry.flushedEvents)
        }

    @Test
    fun `a request's worth waiting is sent without waiting for a pause`() =
        runTest {
            val telemetry = FakeTelemetryRepository()
            val scope = TestScope(testScheduler)
            val recorder = recorder(telemetry, scope)
            recorder.begin("file-1")
            recorder.firstFrame(audioTrackIndex = null)

            repeat(TelemetryRecorder.FLUSH_AT.toInt() - 2) { recorder.stalled(durationMs = 1_000L) }
            scope.runCurrent()
            assertEquals("one short of a request's worth", 0, telemetry.flushCount)

            recorder.stalled(durationMs = 1_000L)
            scope.runCurrent()
            assertEquals(1, telemetry.flushCount)
        }
}
