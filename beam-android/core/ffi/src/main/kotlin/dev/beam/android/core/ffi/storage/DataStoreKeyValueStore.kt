package dev.beam.android.core.ffi.storage

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.first
import uniffi.beam_client_core.KeyValueStore
import uniffi.beam_client_core.StorageException

/**
 * The platform half of the core's storage port.
 *
 * The core owns *what* is persisted and Android owns *where*, which is what
 * lets the core's own tests run against an in-memory fake with no Android on
 * the classpath at all.
 *
 * Plaintext values go to DataStore. Secrets go to DataStore too, but encrypted
 * by [SecretCipher] first, so the session cookie is never at rest in the clear.
 */
internal class DataStoreKeyValueStore(
    private val dataStore: DataStore<Preferences>,
) : KeyValueStore {

    override suspend fun `get`(key: String): String? = read(plainKey(key))

    override suspend fun `put`(key: String, value: String) {
        write(plainKey(key), value)
    }

    override suspend fun `remove`(key: String) {
        delete(plainKey(key))
    }

    override suspend fun listKeys(prefix: String): List<String> = guard {
        dataStore.data.first()
            .asMap()
            .keys
            .map { it.name }
            .filter { it.startsWith(PLAIN_PREFIX) }
            .map { it.removePrefix(PLAIN_PREFIX) }
            .filter { it.startsWith(prefix) }
    }

    override suspend fun getSecret(key: String): String? =
        read(secretKey(key))?.let(SecretCipher::decrypt)

    override suspend fun putSecret(key: String, value: String) {
        write(secretKey(key), SecretCipher.encrypt(value))
    }

    override suspend fun removeSecret(key: String) {
        delete(secretKey(key))
    }

    private suspend fun read(key: Preferences.Key<String>): String? =
        guard { dataStore.data.first()[key] }

    private suspend fun write(key: Preferences.Key<String>, value: String) {
        guard { dataStore.edit { it[key] = value } }
    }

    private suspend fun delete(key: Preferences.Key<String>) {
        guard { dataStore.edit { it.remove(key) } }
    }

    /**
     * Translate a platform failure into the core's own storage error.
     *
     * The core's contract is that storage fails with [StorageException]; an
     * IOException escaping here would cross the FFI boundary as an unexpected
     * panic rather than as the error the calling screen knows how to render.
     */
    private inline fun <T> guard(block: () -> T): T = try {
        block()
    } catch (error: Exception) {
        throw StorageException.Unavailable(error.message ?: error.javaClass.simpleName)
    }

    private companion object {
        const val PLAIN_PREFIX = "plain:"
        const val SECRET_PREFIX = "secret:"

        fun plainKey(key: String) = stringPreferencesKey("$PLAIN_PREFIX$key")
        fun secretKey(key: String) = stringPreferencesKey("$SECRET_PREFIX$key")
    }
}
