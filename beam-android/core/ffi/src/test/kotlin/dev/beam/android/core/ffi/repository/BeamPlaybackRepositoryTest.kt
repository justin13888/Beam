package dev.beam.android.core.ffi.repository

import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.NoHandle
import uniffi.beam_client_core.ProgressDrain
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState

/**
 * How a progress drain reaches the server list the shell watches.
 *
 * The core names a server whose session it refused mid-drain in
 * `expiredServerIds` instead of throwing, so the repository is the only place
 * that can tell the shell the server signed out. The subject is
 * [BeamPlaybackRepository] over the real [ServerChanges]; only the core's two
 * answers are canned, because the real core cannot be made to refuse a
 * session without a server to sign in to.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BeamPlaybackRepositoryTest {
    @Test
    fun `a drain naming an expired server re-publishes the list`() =
        runTest {
            val client = CannedClient(drain(expired = listOf(HOME.id)), listOf(HOME))
            val changes = ServerChanges(client)
            val published = mutableListOf<List<ServerSummary>>()
            backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) { changes.servers.toList(published) }

            val answer = BeamPlaybackRepository(client, changes).drainProgress()

            assertEquals("the drain's own answer is returned", listOf(HOME.id), answer.expiredServerIds)
            assertEquals(
                "the shell learns the server signed out",
                listOf(listOf(HOME)),
                published,
            )
        }

    @Test
    fun `a drain naming no expired server publishes nothing`() =
        runTest {
            val client = CannedClient(drain(expired = emptyList()), listOf(HOME))
            val changes = ServerChanges(client)
            val published = mutableListOf<List<ServerSummary>>()
            backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) { changes.servers.toList(published) }

            BeamPlaybackRepository(client, changes).drainProgress()

            assertEquals(emptyList<List<ServerSummary>>(), published)
        }

    /** The core, answering one drain and one registry read. */
    private class CannedClient(
        private val drain: ProgressDrain,
        private val servers: List<ServerSummary>,
    ) : BeamClient(NoHandle) {
        override suspend fun drainProgress(): ProgressDrain = drain

        override fun listServers(): List<ServerSummary> = servers
    }

    private companion object {
        val HOME =
            ServerSummary(
                id = "home",
                displayName = "Home",
                baseUrl = "https://home.beam.test",
                state = SessionState.Expired,
                isActive = true,
            )

        fun drain(expired: List<String>) =
            ProgressDrain(sent = 0u, pending = 1u, retryInSecs = null, expiredServerIds = expired)
    }
}
