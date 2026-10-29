package dev.beam.android.sync

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import dagger.hilt.android.qualifiers.ApplicationContext
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import javax.inject.Inject

/** Whether the device has a network to reach a server over. */
internal interface NetworkAvailability {
    /**
     * Whether a default network is up: the current answer first, then every
     * change to it.
     */
    val isAvailable: Flow<Boolean>
}

/** [NetworkAvailability] from the platform's default-network callback. */
internal class ConnectivityNetworkAvailability
    @Inject
    constructor(
        @ApplicationContext private val context: Context,
    ) : NetworkAvailability {
        override val isAvailable: Flow<Boolean> =
            callbackFlow {
                val manager = context.getSystemService(ConnectivityManager::class.java)
                val callback =
                    object : ConnectivityManager.NetworkCallback() {
                        override fun onAvailable(network: Network) {
                            trySend(true)
                        }

                        // Losing one network while switching to another is not
                        // being offline, so the answer is read again rather
                        // than assumed.
                        override fun onLost(network: Network) {
                            trySend(manager.activeNetwork != null)
                        }
                    }
                // Sent before registering, because the callback says nothing
                // at all while there is no network: without it, a start with
                // none would never have a first answer to change from.
                trySend(manager.activeNetwork != null)
                manager.registerDefaultNetworkCallback(callback)
                awaitClose { manager.unregisterNetworkCallback(callback) }
            }.distinctUntilChanged()
    }
