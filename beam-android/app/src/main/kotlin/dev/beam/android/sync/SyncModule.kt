package dev.beam.android.sync

import dagger.Binds
import dagger.Module
import dagger.hilt.InstallIn
import dagger.hilt.components.SingletonComponent

@Module
@InstallIn(SingletonComponent::class)
internal interface SyncModule {
    @Binds
    fun networkAvailability(impl: ConnectivityNetworkAvailability): NetworkAvailability
}
