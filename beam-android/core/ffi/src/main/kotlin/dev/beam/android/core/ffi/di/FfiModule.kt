package dev.beam.android.core.ffi.di

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.preferencesDataStoreFile
import dagger.Binds
import dagger.Module
import dagger.Provides
import dagger.hilt.InstallIn
import dagger.hilt.android.qualifiers.ApplicationContext
import dagger.hilt.components.SingletonComponent
import dev.beam.android.core.ffi.preferences.DataStorePreferencesRepository
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.AdminRepository
import dev.beam.android.core.ffi.repository.BeamAdminRepository
import dev.beam.android.core.ffi.repository.BeamCatalogRepository
import dev.beam.android.core.ffi.repository.BeamPlaybackRepository
import dev.beam.android.core.ffi.repository.BeamServerRepository
import dev.beam.android.core.ffi.repository.BeamSessionRepository
import dev.beam.android.core.ffi.repository.BeamTelemetryRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.repository.SessionRepository
import dev.beam.android.core.ffi.repository.TelemetryRepository
import dev.beam.android.core.ffi.storage.DataStoreKeyValueStore
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.KeyValueStore
import javax.inject.Singleton

/**
 * Everything the core needs to exist, and the repositories over it.
 *
 * Public, with [FfiBindsModule], so a test in another module can name both in
 * `@TestInstallIn(replaces = ...)`: the app's Hilt tests swap the real
 * repositories for the `:core:testing` fakes, because these sit over a
 * `BeamClient` that talks to a server, which a test has none of.
 */
@Module
@InstallIn(SingletonComponent::class)
public object FfiProvidesModule {
    @Provides
    @Singleton
    public fun dataStore(
        @ApplicationContext context: Context,
    ): DataStore<Preferences> =
        PreferenceDataStoreFactory.create {
            context.preferencesDataStoreFile("beam")
        }

    @Provides
    @Singleton
    public fun keyValueStore(dataStore: DataStore<Preferences>): KeyValueStore = DataStoreKeyValueStore(dataStore)

    /**
     * The core itself.
     *
     * A singleton because it owns the per-server HTTP clients, the session
     * cookies, and the queued progress and telemetry. Building a second one
     * would split the queues between two copies and give them different ideas
     * about which server is active.
     */
    @Provides
    @Singleton
    public fun beamClient(storage: KeyValueStore): BeamClient = BeamClient(storage)
}

/**
 * Binds each repository interface to the implementation over the core.
 *
 * Public for the same reason as [FfiProvidesModule]. An abstract class rather
 * than an interface because the implementations are internal to this module:
 * each binding is an internal member of a public type, so the module can be
 * replaced by name without the implementations leaving `:core:ffi`.
 */
@Module
@InstallIn(SingletonComponent::class)
public abstract class FfiBindsModule {
    @Binds
    internal abstract fun serverRepository(impl: BeamServerRepository): ServerRepository

    @Binds
    internal abstract fun catalogRepository(impl: BeamCatalogRepository): CatalogRepository

    @Binds
    internal abstract fun playbackRepository(impl: BeamPlaybackRepository): PlaybackRepository

    @Binds
    internal abstract fun sessionRepository(impl: BeamSessionRepository): SessionRepository

    @Binds
    internal abstract fun adminRepository(impl: BeamAdminRepository): AdminRepository

    @Binds
    internal abstract fun telemetryRepository(impl: BeamTelemetryRepository): TelemetryRepository

    @Binds
    internal abstract fun preferencesRepository(impl: DataStorePreferencesRepository): PreferencesRepository
}
