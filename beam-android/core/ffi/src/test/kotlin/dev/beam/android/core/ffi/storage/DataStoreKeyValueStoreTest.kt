package dev.beam.android.core.ffi.storage

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.fail
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import uniffi.beam_client_core.KeyValueStore
import uniffi.beam_client_core.StorageException
import java.io.IOException

/**
 * The platform half of the core's storage port, over a real DataStore file.
 *
 * Under Robolectric for `android.util.Base64`, which the cipher's encoding
 * uses. The cipher itself is the production one over a software key.
 */
@RunWith(RobolectricTestRunner::class)
class DataStoreKeyValueStoreTest : KeyValueStoreContract() {
    @get:Rule
    val files = DataStoreFiles()

    override fun newStore(): KeyValueStore = DataStoreKeyValueStore(files.open(), softwareCipher())

    @Test
    fun `a secret survives a new instance over the same file`() =
        runTest {
            val key = softwareKey()
            DataStoreKeyValueStore(files.open(), softwareCipher(key)).putSecret("session/a", "cookie")

            val restarted = DataStoreKeyValueStore(files.reopen(), softwareCipher(key))

            assertEquals("cookie", restarted.getSecret("session/a"))
        }

    @Test
    fun `a secret is never at rest in the clear`() =
        runTest {
            val dataStore = files.open()
            DataStoreKeyValueStore(dataStore, softwareCipher()).putSecret("session/a", "opaque-cookie")

            val stored =
                dataStore.data
                    .first()
                    .asMap()
                    .values
                    .map { it.toString() }
            assertEquals(1, stored.size)
            assertFalse("the cookie is encrypted before it is written", stored.single().contains("opaque-cookie"))
        }

    @Test
    fun `a corrupt secret reads as absent`() =
        runTest {
            // Garbage, a value too short to hold an IV, and a well-formed
            // ciphertext under a key that has since been replaced -- which is
            // what a new lock screen or a restore onto another phone leaves.
            // Each one is a session to sign in again, never a crash.
            val dataStore = files.open()
            val store = DataStoreKeyValueStore(dataStore, softwareCipher())
            val otherKey = softwareCipher().encrypt("cookie")
            val stored =
                mapOf(
                    "garbage" to "not base64 at all!",
                    "short" to "AAAA",
                    "rekeyed" to otherKey,
                )
            dataStore.edit { preferences ->
                stored.forEach { (name, value) -> preferences[stringPreferencesKey("secret:$name")] = value }
            }

            for (name in stored.keys) {
                assertNull("$name reads as absent", store.getSecret(name))
            }
        }

    @Test
    fun `a store that cannot be read or written fails with the core's storage error`() =
        runTest {
            val store = DataStoreKeyValueStore(UnreadableDataStore(), softwareCipher())
            val calls: List<Pair<String, suspend () -> Unit>> =
                listOf(
                    "get" to { store.get("k") },
                    "put" to { store.put("k", "v") },
                    "remove" to { store.remove("k") },
                    "listKeys" to { store.listKeys("") },
                    "getSecret" to { store.getSecret("k") },
                    "putSecret" to { store.putSecret("k", "v") },
                    "removeSecret" to { store.removeSecret("k") },
                )

            for ((name, call) in calls) {
                try {
                    call()
                    fail("$name succeeded on an unreadable store")
                } catch (failure: StorageException.Unavailable) {
                    // An IOException escaping instead would cross the FFI as
                    // a panic rather than as an error a screen can render.
                    assertEquals("$name carries the cause", "disk is gone", failure.detail)
                }
            }
        }

    /** A DataStore whose file has become unreadable: every read and write throws. */
    private class UnreadableDataStore : DataStore<Preferences> {
        override val data: Flow<Preferences> = flow { throw IOException("disk is gone") }

        override suspend fun updateData(transform: suspend (t: Preferences) -> Preferences): Preferences =
            throw IOException("disk is gone")
    }
}
