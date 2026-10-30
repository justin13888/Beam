package dev.beam.android.di

import androidx.media3.common.util.UnstableApi
import androidx.media3.exoplayer.offline.DownloadManager
import dagger.Module
import dagger.Provides
import dagger.hilt.components.SingletonComponent
import dagger.hilt.testing.TestInstallIn
import dev.beam.android.core.media.di.MediaBindsModule
import dev.beam.android.core.media.download.DownloadManagerHolder
import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.model.DownloadRecord
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import javax.inject.Singleton

/**
 * Downloads, replaced for every Hilt test in the app.
 *
 * The real repository builds Media3's download manager over the native core,
 * which Robolectric cannot load. Nothing else in `:core:media` needs
 * replacing: its bindings are built only when something asks for them, and
 * the player and the download service are never started here.
 */
@Module
@TestInstallIn(
    components = [SingletonComponent::class],
    replaces = [MediaBindsModule::class],
)
object FakeDownloadsModule {
    @Provides
    @Singleton
    fun downloads(): DownloadRepository = NoDownloads()

    /**
     * Only the download service asks for the manager, and no test here starts
     * it. Failing loudly says so if one ever does, rather than handing it a
     * manager over nothing.
     */
    @Provides
    @OptIn(UnstableApi::class)
    fun managerHolder(): DownloadManagerHolder =
        object : DownloadManagerHolder {
            override fun manager(): DownloadManager =
                throw UnsupportedOperationException("no download service under test")
        }
}

/** A device with nothing downloaded, which ignores every request to change that. */
private class NoDownloads : DownloadRepository {
    override val downloads: Flow<List<DownloadRecord>> = MutableStateFlow(emptyList())

    override suspend fun enqueue(
        fileId: String,
        serverId: String,
        mediaId: String,
        title: String,
        subtitle: String?,
        posterUrl: String?,
    ) = Unit

    override suspend fun pause(fileId: String) = Unit

    override suspend fun resume(fileId: String) = Unit

    override suspend fun remove(fileId: String) = Unit

    override suspend fun removeAll() = Unit

    override suspend fun setRequiresUnmeteredNetwork(required: Boolean) = Unit

    override suspend fun isDownloaded(fileId: String): Boolean = false
}
