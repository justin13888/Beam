package dev.beam.android.feature.settings

import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.FakeSessionRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException

/** Forgetting a server takes the downloads fetched from it, and nothing else. */
@OptIn(ExperimentalCoroutinesApi::class)
class ServerDownloadsTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    /** Home is active; cabin is known and not in use, so it can be forgotten. */
    private val servers =
        FakeServerRepository(
            listOf(
                Fixtures.server(id = "home", displayName = "Home"),
                Fixtures.server(id = "cabin", displayName = "Cabin", isActive = false),
            ),
            restored = true,
        )

    private val downloads =
        FakeDownloadRepository(
            listOf(download("h1", serverId = "home"), download("c1", serverId = "cabin")),
        )

    private fun viewModel() =
        SettingsViewModel(FakePreferencesRepository(), servers, FakeSessionRepository(), downloads)

    @Test
    fun `forgetting a server deletes its downloads and only its downloads`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            model.removeServer("cabin")
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("h1"), downloads.current.map { it.fileId })
        }

    @Test
    fun `a removal the core reports as failed still deletes its downloads`() =
        runTest {
            val model = viewModel()
            testScheduler.advanceUntilIdle()

            // The core's forget_server unlists the server before any storage
            // write that can fail, so a failed removal has already forgotten
            // it; downloads kept back would belong to no listed server.
            servers.failWith = BeamException.Network("the connection dropped", true)
            model.removeServer("cabin")
            testScheduler.advanceUntilIdle()

            assertEquals(listOf("h1"), downloads.current.map { it.fileId })
        }
}
