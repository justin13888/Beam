// Media3's cache is @UnstableApi, like the rest of its DataSource surface.
@file:UnstableApi

package dev.beam.android.core.media.player

import android.net.Uri
import androidx.media3.common.util.UnstableApi
import androidx.media3.database.StandaloneDatabaseProvider
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.cache.CacheDataSource
import androidx.media3.datasource.cache.CacheWriter
import androidx.media3.datasource.cache.NoOpCacheEvictor
import androidx.media3.datasource.cache.SimpleCache
import org.junit.rules.ExternalResource
import org.junit.rules.TemporaryFolder
import org.robolectric.RuntimeEnvironment

/**
 * A download cache in a temporary directory, built as the app builds its own,
 * and released after each test.
 */
class DownloadCacheRule : ExternalResource() {
    private val folder = TemporaryFolder()
    private lateinit var database: StandaloneDatabaseProvider
    lateinit var cache: SimpleCache
        private set

    override fun before() {
        folder.create()
        database = StandaloneDatabaseProvider(RuntimeEnvironment.getApplication())
        cache = SimpleCache(folder.newFolder("downloads"), NoOpCacheEvictor(), database)
    }

    override fun after() {
        cache.release()
        database.close()
        folder.delete()
    }

    /**
     * Put [bytes] in the cache under [uri], in full, as a completed download
     * of [uri] leaves them.
     */
    fun store(
        uri: String,
        bytes: ByteArray,
    ) {
        CacheWriter(
            CacheDataSource(cache, ByteArrayDataSource(bytes)),
            DataSpec(Uri.parse(uri)),
            null,
            null,
        ).cache()
    }
}
