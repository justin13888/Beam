package dev.beam.android.core.media.di

import android.content.Context
import androidx.media3.datasource.cache.Cache
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import dagger.Binds
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.download.BeamDownloadManager
import dev.beam.android.core.media.download.DownloadManagerHolder
import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.media.download.DownloadTitleStore
import dev.beam.android.core.media.download.FileDownloadTitleStore
import dev.beam.android.core.media.download.MediaDownloadRepository
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.player.BeamPlayer
import dev.beam.android.core.media.player.ExoBeamPlayer
import dev.beam.android.core.media.session.PlayerProvider
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.MainScope
import okhttp3.OkHttpClient
import javax.inject.Singleton

@Module
@InstallIn(SingletonComponent::class)
internal object MediaModule {
    @Provides
    @Singleton
    fun okHttpClient(): OkHttpClient = BeamHttpClientFactory.shared()

    @Provides
    @Singleton
    fun httpClientFactory(client: OkHttpClient): BeamHttpClientFactory = BeamHttpClientFactory(client)

    @Provides
    @Singleton
    fun exoPlayer(
        @ApplicationContext context: Context,
    ): ExoPlayer =
        ExoPlayer
            .Builder(context)
            .setRenderersFactory(
                DefaultRenderersFactory(context)
                    // Software decoders are a fallback, never a preference.
                    // Beam never transcodes (ADR-0004), so a file whose codec
                    // has no hardware decoder is either played in software or
                    // not at all -- and "not at all" is the worse answer for a
                    // file the user already has. The capability check in the
                    // core still steers *selection* towards hardware.
                    .setExtensionRendererMode(
                        DefaultRenderersFactory.EXTENSION_RENDERER_MODE_PREFER,
                    ).setEnableDecoderFallback(true),
            ).setHandleAudioBecomingNoisy(true)
            .build()

    @Provides
    @Singleton
    fun playerProvider(player: ExoPlayer): PlayerProvider =
        object : PlayerProvider {
            override fun exoPlayer(): ExoPlayer = player
        }

    @Provides
    @Singleton
    fun playerScope(): CoroutineScope = MainScope()

    @Provides
    @Singleton
    fun beamPlayer(
        @ApplicationContext context: Context,
        player: ExoPlayer,
        repository: PlaybackRepository,
        clients: BeamHttpClientFactory,
        scope: CoroutineScope,
    ): BeamPlayer = ExoBeamPlayer(context, player, repository, clients, scope)

    @Provides
    @Singleton
    fun downloadCache(
        @ApplicationContext context: Context,
    ): Cache = BeamDownloadManager.cache(context)

    @Provides
    @Singleton
    fun downloadTitles(
        @ApplicationContext context: Context,
    ): DownloadTitleStore = FileDownloadTitleStore(context)
}

@Module
@InstallIn(SingletonComponent::class)
internal interface MediaBindsModule {
    /**
     * One instance serving both roles. The downloads screen and the foreground
     * service must drive the same manager, or the screen would report progress
     * for downloads the service is not running.
     */
    @Binds
    @Singleton
    fun downloadRepository(impl: MediaDownloadRepository): DownloadRepository

    @Binds
    @Singleton
    fun downloadManagerHolder(impl: MediaDownloadRepository): DownloadManagerHolder
}
