package dev.beam.android.core.media.download

import uniffi.beam_client_core.KeyValueStore
import java.util.concurrent.ConcurrentHashMap

/**
 * Storage for a real core in these tests, held in memory.
 *
 * Scaffolding, never the subject: the core's download records are what the
 * tests here read, and the store's own behaviour is the shared contract's
 * concern (`KeyValueStoreContract` in `:core:ffi`).
 */
internal class InMemoryKeyValueStore : KeyValueStore {
    private val plain = ConcurrentHashMap<String, String>()
    private val secret = ConcurrentHashMap<String, String>()

    override suspend fun `get`(key: String): String? = plain[key]

    override suspend fun `put`(
        key: String,
        value: String,
    ) {
        plain[key] = value
    }

    override suspend fun `remove`(key: String) {
        plain.remove(key)
    }

    override suspend fun listKeys(prefix: String): List<String> = plain.keys.filter { it.startsWith(prefix) }

    override suspend fun getSecret(key: String): String? = secret[key]

    override suspend fun putSecret(
        key: String,
        value: String,
    ) {
        secret[key] = value
    }

    override suspend fun removeSecret(key: String) {
        secret.remove(key)
    }
}
