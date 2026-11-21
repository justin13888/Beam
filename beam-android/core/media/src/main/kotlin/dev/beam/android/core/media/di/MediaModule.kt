// Media3 marks most of its ExoPlayer, DataSource and offline surface
// @UnstableApi: the library guarantees behaviour, not source compatibility
// across minor versions. Opting in at the file level is what the library
// itself documents for application code; the protection is the pinned version
// in the catalog, and an upgrade is a deliberate change that recompiles here.
@file:UnstableApi

package dev.beam.android.core.media.di

import android.content.Context
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.cache.Cache
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import coil3.SingletonImageLoader
import dagger.Binds
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.TelemetryRepository
import dev.beam.android.core.media.download.BeamDownloadManager
import dev.beam.android.core.media.download.DownloadArtwork
import dev.beam.android.core.media.download.DownloadCommands
import dev.beam.android.core.media.download.DownloadManagerHolder
import dev.beam.android.core.media.download.DownloadRepository
import dev.beam.android.core.media.download.MediaDownloadRepository
import dev.beam.android.core.media.download.ServiceDownloadCommands
import dev.beam.android.core.media.http.BeamHttpClientFactory
import dev.beam.android.core.media.http.ServerCallFactory
import dev.beam.android.core.media.player.BeamPlayer
import dev.beam.android.core.media.player.ExoBeamPlayer
import dev.beam.android.core.media.session.PlayerProvider
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.MainScope
import okhttp3.OkHttpClient
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.BeamException
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

    /**
     * The image loader's and the download manager's network path. Asks the
     * core on every call for the session of the server the request goes to,
     * active or not, so signing out or signing in again takes effect on the
     * next fetch rather than on the next launch, and a download from one
     * server keeps its session after the viewer switches to another.
     */
    @Provides
    @Singleton
    fun serverCallFactory(
        clients: BeamHttpClientFactory,
        client: BeamClient,
    ): ServerCallFactory =
        ServerCallFactory.signedInServers(
            clients,
            servers = {
                // The core reads the list from memory and never refuses it;
                // this keeps a refusal from failing the fetch all the same.
                try {
                    client.listServers()
                } catch (_: BeamException) {
                    emptyList()
                }
            },
        ) { serverId ->
            // Not signed in to that server, or forgotten since: the request
            // goes out unauthenticated, and the server answers 401 exactly as
            // it would for any other signed-out request.
            try {
                client.serverHttpConfigFor(serverId)
            } catch (_: BeamException) {
                null
            }
        }

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
        telemetry: TelemetryRepository,
        clients: BeamHttpClientFactory,
        scope: CoroutineScope,
    ): BeamPlayer = ExoBeamPlayer(context, player, repository, telemetry, clients, scope)

    @Provides
    @Singleton
    fun downloadCache(
        @ApplicationContext context: Context,
    ): Cache = BeamDownloadManager.cache(context)

    /** Every change to a download goes to the foreground download service. */
    @Provides
    @Singleton
    fun downloadCommands(
        @ApplicationContext context: Context,
    ): DownloadCommands = ServiceDownloadCommands(context)

    /**
     * Fetched through the app's own image loader, so a poster kept for a
     * download goes out under the same session and trust decision as every
     * other poster, then kept in a store of its own outside that loader's cache.
     */
    @Provides
    @Singleton
    fun downloadArtwork(
        @ApplicationContext context: Context,
    ): DownloadArtwork = DownloadArtwork(context) { SingletonImageLoader.get(context) }
}

/**
 * The download bindings.
 *
 * Public so a test in another module can replace it by name: the manager
 * behind it is built over the native core, which a Robolectric test cannot
 * load. An abstract class with internal members, as `FfiBindsModule` is, so
 * [MediaDownloadRepository] stays internal to this module. Nothing in
 * [MediaModule] needs replacing -- each binding there is built only when
 * something asks for it -- so it stays internal.
 */
@Module
@InstallIn(SingletonComponent::class)
public abstract class MediaBindsModule {
    /**
     * One instance serving both roles. The downloads screen and the foreground
     * service must drive the same manager, or the screen would report progress
     * for downloads the service is not running.
     */
    @Binds
    @Singleton
    internal abstract fun downloadRepository(impl: MediaDownloadRepository): DownloadRepository

    @Binds
    @Singleton
    internal abstract fun downloadManagerHolder(impl: MediaDownloadRepository): DownloadManagerHolder
}
