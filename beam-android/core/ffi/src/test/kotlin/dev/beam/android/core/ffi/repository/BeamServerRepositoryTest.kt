package dev.beam.android.core.ffi.repository

import dev.beam.android.core.ffi.storage.DataStoreFiles
import dev.beam.android.core.ffi.storage.DataStoreKeyValueStore
import dev.beam.android.core.ffi.storage.softwareCipher
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.toList
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.BeforeClass
import org.junit.Rule
import org.junit.Test
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.ServerSummary
import uniffi.beam_client_core.SessionState
import java.io.File

/**
 * The server registry through the real core: the generated UniFFI bindings,
 * over JNA, into the host build of `libbeam_client_core`, storing through the
 * DataStore store a phone uses. Nothing here is a double.
 *
 * Plain JUnit: the flow needs no Android framework class, so there is no
 * Robolectric sandbox to start. It stays clear of secrets, the one
 * part of the store that needs the platform's `Base64`; those are covered by
 * `DataStoreKeyValueStoreTest`.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class BeamServerRepositoryTest {
    @get:Rule
    val files = DataStoreFiles()

    @Test
    fun `the registry round trips through the bindings and datastore`() =
        runTest {
            val client = BeamClient(DataStoreKeyValueStore(files.open(), softwareCipher()))
            val repository = BeamServerRepository(client, ServerChanges(client))
            val published = mutableListOf<List<ServerSummary>>()
            backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) { repository.servers.toList(published) }

            assertEquals("nothing is published before the registry is read", 0, published.size)
            assertEquals(emptyList<ServerSummary>(), repository.restore())
            assertEquals("restoring publishes what it read", listOf(emptyList<ServerSummary>()), published)

            val added = repository.addServer("beam.example.com", "Home")
            assertEquals(listOf(added), published.last())
            assertTrue("a new server is the active one", added.isActive)
            assertEquals(SessionState.LoggedOut, added.state)

            repository.trustCertificate(added.id, FINGERPRINT)
            assertEquals("trusting a certificate changes nothing listed, so nothing is published", 2, published.size)
            assertEquals(listOf(FINGERPRINT), repository.trustedCertificates(added.id))
            client.close()

            // A cold start: a new core over a new DataStore on the same file.
            val restarted = BeamClient(DataStoreKeyValueStore(files.reopen(), softwareCipher()))
            val again = BeamServerRepository(restarted, ServerChanges(restarted))

            assertEquals(listOf(added), again.restore())
            assertEquals("the pin outlives the process", listOf(FINGERPRINT), again.trustedCertificates(added.id))
            assertEquals(added, again.activeServer())
            restarted.close()
        }

    @Test
    fun `adding the same address twice lists one server`() =
        runTest {
            val client = BeamClient(DataStoreKeyValueStore(files.open(), softwareCipher()))
            val repository = BeamServerRepository(client, ServerChanges(client))
            repository.restore()

            val first = repository.addServer("beam.example.com", null)
            val second = repository.addServer("https://beam.example.com/", null)

            assertEquals(first.id, second.id)
            assertEquals(listOf(first.id), client.listServers().map { it.id })
            client.close()
        }

    @Test
    fun `forgetting a server is published and survives a restart`() =
        runTest {
            val client = BeamClient(DataStoreKeyValueStore(files.open(), softwareCipher()))
            val repository = BeamServerRepository(client, ServerChanges(client))
            val published = mutableListOf<List<ServerSummary>>()
            backgroundScope.launch(UnconfinedTestDispatcher(testScheduler)) { repository.servers.toList(published) }
            repository.restore()
            val home = repository.addServer("beam.example.com", "Home")
            val cabin = repository.addServer("cabin.example.com", "Cabin")

            repository.removeServer(home.id)

            assertEquals(listOf(cabin.id), published.last().map { it.id })
            client.close()
            val restarted = BeamClient(DataStoreKeyValueStore(files.reopen(), softwareCipher()))
            assertEquals(listOf(cabin.id), restarted.restore().map { it.id })
            restarted.close()
        }

    private companion object {
        const val FINGERPRINT = "AA:BB:CC:DD"

        /**
         * Fails, never skips, when the host library is missing: a test that
         * quietly does not run is indistinguishable from one that passes.
         * Gradle builds it (`:core:ffi:hostCoreLibrary`) and points
         * `jna.library.path` at it; outside Gradle, run `mise run core:host`.
         */
        @BeforeClass
        @JvmStatic
        fun theHostCoreIsBuilt() {
            val directory = System.getProperty("jna.library.path")
            if (directory == null) {
                fail("jna.library.path is unset; run the tests through Gradle")
                return
            }
            val built =
                File(directory).listFiles().orEmpty().any { it.name.startsWith("libbeam_client_core.") }
            if (!built) fail("no libbeam_client_core in $directory; run `mise run core:host`")
        }
    }
}
