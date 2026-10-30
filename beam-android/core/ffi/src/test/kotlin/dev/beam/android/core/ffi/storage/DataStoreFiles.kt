package dev.beam.android.core.ffi.storage

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.job
import kotlinx.coroutines.runBlocking
import org.junit.rules.ExternalResource
import org.junit.rules.TemporaryFolder
import java.io.File
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey

/**
 * Real Preferences DataStores over real files in a temporary directory: the
 * filesystem needs no infrastructure, so the stores are tested against what
 * they actually write rather than against a fake of DataStore.
 *
 * DataStore allows one active instance per file in a process, so [reopen]
 * closes the previous instance's scope before opening the file again -- which
 * is exactly what a process restart does to the app's own store.
 */
class DataStoreFiles : ExternalResource() {
    private val folder = TemporaryFolder()
    private val scopes = mutableListOf<CoroutineScope>()
    private var file: File? = null

    override fun before() {
        folder.create()
    }

    override fun after() {
        scopes.forEach { it.cancel() }
        folder.delete()
    }

    /** A store over a fresh file. */
    fun open(): DataStore<Preferences> {
        file = File(folder.newFolder(), "beam.preferences_pb")
        return create()
    }

    /** A new store over the file [open] last created, as after a restart. */
    fun reopen(): DataStore<Preferences> {
        // Joined, not just cancelled: the file stays claimed until the old
        // instance's scope has actually completed.
        runBlocking { scopes.forEach { it.coroutineContext.job.cancelAndJoin() } }
        scopes.clear()
        return create()
    }

    private fun create(): DataStore<Preferences> {
        val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
        scopes += scope
        val target = checkNotNull(file) { "open() a store first" }
        return PreferenceDataStoreFactory.create(scope = scope) { target }
    }
}

/**
 * The production cipher over a software AES key, because the Android keystore
 * does not exist on the JVM. Everything but where the key lives is the code a
 * phone runs.
 */
internal fun softwareCipher(key: SecretKey = softwareKey()): SecretCipher = SecretCipher { key }

internal fun softwareKey(): SecretKey = KeyGenerator.getInstance("AES").apply { init(256) }.generateKey()
