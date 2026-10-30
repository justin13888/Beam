package dev.beam.android.core.ffi.storage

import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.beam_client_core.KeyValueStore

/**
 * The core's shared behavioural contract for its storage port, over a
 * platform store.
 *
 * Case for case the `key_value_store_contract!` macro in
 * `beam-client-core/src/ports/contract.rs`, which constrains the core's
 * in-memory double. The same assertions holding here is what makes that
 * double a faithful stand-in for the store a phone actually uses; a case
 * added there belongs here too.
 */
abstract class KeyValueStoreContract {
    /** A fresh, empty store. */
    abstract fun newStore(): KeyValueStore

    @Test
    fun `a written value reads back and a second write replaces it`() =
        runTest {
            val store = newStore()
            store.put("servers/index", "[]")
            assertEquals("[]", store.get("servers/index"))

            store.put("servers/index", "[\"a\"]")
            assertEquals("[\"a\"]", store.get("servers/index"))
        }

    @Test
    fun `an absent key reads as none and removing it succeeds`() =
        runTest {
            val store = newStore()
            assertNull(store.get("nothing"))
            store.remove("nothing")
            assertNull(store.getSecret("nothing"))
            store.removeSecret("nothing")
        }

    @Test
    fun `a removed value reads as none`() =
        runTest {
            val store = newStore()
            store.put("k", "v")
            store.remove("k")
            assertNull(store.get("k"))
        }

    @Test
    fun `a secret reads back, is replaced and is removed`() =
        runTest {
            val store = newStore()
            store.putSecret("session/a", "first")
            store.putSecret("session/a", "second")
            assertEquals("second", store.getSecret("session/a"))

            store.removeSecret("session/a")
            assertNull(store.getSecret("session/a"))
        }

    /**
     * The whole point of the split boundary: a secret is not reachable
     * through the plaintext methods, and a plaintext value is not reachable
     * through the secret ones, under the same key.
     */
    @Test
    fun `secrets and plaintext are separate namespaces`() =
        runTest {
            val store = newStore()
            store.putSecret("session/a", "cookie")
            assertNull(store.get("session/a"))

            store.put("servers/a", "record")
            assertNull(store.getSecret("servers/a"))

            // Removing one never removes the other.
            store.put("both", "plain")
            store.putSecret("both", "secret")
            store.remove("both")
            assertEquals("secret", store.getSecret("both"))
        }

    @Test
    fun `list keys is every live plaintext key with the prefix`() =
        runTest {
            val store = newStore()
            store.put("servers/a", "1")
            store.put("servers/b", "2")
            store.put("servers/gone", "3")
            store.remove("servers/gone")
            store.put("pins/a", "4")
            store.putSecret("servers/secret", "s")

            assertEquals(listOf("servers/a", "servers/b"), store.listKeys("servers/").sorted())
            assertTrue(store.listKeys("absent/").isEmpty())
        }
}
