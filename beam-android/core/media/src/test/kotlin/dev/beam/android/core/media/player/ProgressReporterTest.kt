package dev.beam.android.core.media.player

import dev.beam.android.core.testing.FakePlaybackRepository
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.beam_client_core.BeamException

@OptIn(ExperimentalCoroutinesApi::class)
class ProgressReporterTest {
    @Test
    fun `nothing is reported before the first interval elapses`() =
        runTest {
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.start("file-1") { PlaybackPosition(positionMs = 1_000L, durationMs = 60_000L) }
            scope.advanceTimeBy(14_000L)
            scope.runCurrent()

            assertTrue(repository.reportedProgress.isEmpty())
            reporter.stop()
        }

    @Test
    fun `a position is reported once per interval`() =
        runTest {
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.start("file-1") { PlaybackPosition(positionMs = 30_000L, durationMs = 60_000L) }
            scope.advanceTimeBy(46_000L)
            scope.runCurrent()

            assertEquals(3, repository.reportedProgress.size)
            reporter.stop()
        }

    @Test
    fun `milliseconds are converted to the seconds the server expects`() =
        runTest {
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.forceReport("file-1", PlaybackPosition(positionMs = 90_500L, durationMs = 3_600_000L))
            scope.runCurrent()

            val sent = repository.reportedProgress.single()
            assertEquals(90.5, sent.positionSecs, 0.001)
            assertEquals(3_600.0, sent.durationSecs!!, 0.001)
        }

    @Test
    fun `a forced report is marked as forced so the core bypasses its throttle`() =
        runTest {
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.forceReport("file-1", PlaybackPosition(positionMs = 5_000L, durationMs = null))
            scope.runCurrent()

            assertTrue(repository.reportedProgress.single().force)
        }

    @Test
    fun `an unknown duration is sent as absent rather than as zero`() =
        runTest {
            // Zero would tell the server the title has no length, which it would
            // reasonably store; the resume bar would then be wrong for every later
            // session.
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.forceReport("file-1", PlaybackPosition(positionMs = 5_000L, durationMs = 0L))
            scope.runCurrent()

            assertEquals(null, repository.reportedProgress.single().durationSecs)
        }

    @Test
    fun `stopping halts the interval`() =
        runTest {
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.start("file-1") { PlaybackPosition(positionMs = 1_000L, durationMs = 60_000L) }
            scope.advanceTimeBy(16_000L)
            scope.runCurrent()
            val afterFirst = repository.reportedProgress.size

            reporter.stop()
            scope.advanceTimeBy(60_000L)
            scope.runCurrent()

            assertEquals(afterFirst, repository.reportedProgress.size)
        }

    @Test
    fun `a sample that cannot be taken is skipped rather than reported as zero`() =
        runTest {
            val repository = FakePlaybackRepository()
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.start("file-1") { null }
            scope.advanceTimeBy(46_000L)
            scope.runCurrent()

            assertTrue(repository.reportedProgress.isEmpty())
            reporter.stop()
        }

    @Test
    fun `a failure to send does not stop playback reporting`() =
        runTest {
            // The core queues an undeliverable position durably and drains it
            // later, so a throw here would interrupt playback for something
            // already handled.
            val repository =
                FakePlaybackRepository().apply {
                    failWith = BeamException.Network("offline", retryable = true)
                }
            val scope = TestScope(testScheduler)
            val reporter = ProgressReporter(repository, scope)

            reporter.start("file-1") { PlaybackPosition(positionMs = 1_000L, durationMs = 60_000L) }
            scope.advanceTimeBy(46_000L)
            scope.runCurrent()

            reporter.stop()
        }
}
