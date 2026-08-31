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
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.ffi.repository.ServerRepository
import dev.beam.android.core.ffi.repository.SessionRepository
import dev.beam.android.core.ffi.storage.DataStoreKeyValueStore
import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.KeyValueStore
import javax.inject.Singleton

/** Everything the core needs to exist, and the repositories over it. */
@Module
@InstallIn(SingletonComponent::class)
internal object FfiProvidesModule {
    @Provides
    @Singleton
    fun dataStore(
        @ApplicationContext context: Context,
    ): DataStore<Preferences> =
        PreferenceDataStoreFactory.create {
            context.preferencesDataStoreFile("beam")
        }

    @Provides
    @Singleton
    fun keyValueStore(dataStore: DataStore<Preferences>): KeyValueStore = DataStoreKeyValueStore(dataStore)

    /**
     * The core itself.
     *
     * A singleton because it owns the per-server HTTP clients, the session
     * cookies, and the metadata cache. Building a second one would silently
     * halve the cache hit rate and give the two copies different ideas about
     * which server is active.
     */
    @Provides
    @Singleton
    fun beamClient(storage: KeyValueStore): BeamClient = BeamClient(storage)
}

/** Binds each repository interface to the implementation over the core. */
@Module
@InstallIn(SingletonComponent::class)
internal interface FfiBindsModule {
    @Binds
    fun serverRepository(impl: BeamServerRepository): ServerRepository

    @Binds
    fun catalogRepository(impl: BeamCatalogRepository): CatalogRepository

    @Binds
    fun playbackRepository(impl: BeamPlaybackRepository): PlaybackRepository

    @Binds
    fun sessionRepository(impl: BeamSessionRepository): SessionRepository

    @Binds
    fun adminRepository(impl: BeamAdminRepository): AdminRepository

    @Binds
    fun preferencesRepository(impl: DataStorePreferencesRepository): PreferencesRepository
}
