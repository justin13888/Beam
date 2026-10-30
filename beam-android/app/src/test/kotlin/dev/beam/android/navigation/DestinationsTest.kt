package dev.beam.android.navigation

import android.app.Application
import android.os.Bundle
import android.os.Parcel
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.serialization.NavBackStackSerializer
import androidx.navigation3.runtime.serialization.NavKeySerializer
import androidx.savedstate.serialization.decodeFromSavedState
import androidx.savedstate.serialization.encodeToSavedState
import kotlinx.serialization.ExperimentalSerializationApi
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The back stack survives what Android does to an app in the background: it
 * is written into the saved-instance state, parcelled out of the process, and
 * read back into a new one.
 *
 * Through the serializer `rememberNavBackStack` itself saves with, so a
 * destination that cannot make the trip -- one not marked `@Serializable`, or
 * carrying a field that is not -- fails here rather than as a crash the first
 * time a phone reclaims the app while it is open on that screen.
 *
 * Robolectric for the real `Bundle` and `Parcel`; a plain [Application]
 * keeps the app's own, which loads the native core, out of the test.
 */
@RunWith(RobolectricTestRunner::class)
@Config(application = Application::class)
class DestinationsTest {
    @Test
    fun `a destination round trips through saved state`() {
        // One of every destination, each field off its default so a field
        // that is dropped on the way comes back different.
        val stack =
            NavBackStack<NavKey>(
                SignIn,
                *TopLevel.entries.toTypedArray(),
                MediaDetail(mediaId = "movie-1"),
                LibraryDetail(libraryId = "library-1"),
                History,
                Admin,
                Player(
                    fileId = "file-1",
                    mediaId = "show-1",
                    episodeId = "episode-4",
                    title = "Le Bureau",
                    startPositionSecs = 1_234.5,
                    isSourcePicked = true,
                ),
            )
        val serializer = NavBackStackSerializer(NavKeySerializer<NavKey>())

        val restored = decodeFromSavedState(serializer, throughAParcel(encodeToSavedState(serializer, stack)))

        assertEquals(stack.toList(), restored.toList())
        assertEquals(
            "the stack holds one of every destination the sealed hierarchy declares",
            everyDestination(),
            stack.map { it::class.java.name }.toSet(),
        )
    }

    /**
     * Every subclass of [Destination], by serial name -- the class name, as
     * none declares its own -- read from the sealed serializer rather than
     * listed by hand, so a new destination is one this test must be given.
     */
    @OptIn(ExperimentalSerializationApi::class)
    private fun everyDestination(): Set<String> {
        // A sealed descriptor is the pair (type discriminator, value); the
        // value's elements are the subclasses.
        val subclasses = Destination.serializer().descriptor.getElementDescriptor(1)
        return (0 until subclasses.elementsCount).map(subclasses::getElementName).toSet()
    }

    /** What leaving the process does to saved state: marshalled to bytes and back. */
    private fun throughAParcel(state: Bundle): Bundle {
        val outgoing = Parcel.obtain()
        val bytes =
            try {
                outgoing.writeBundle(state)
                outgoing.marshall()
            } finally {
                outgoing.recycle()
            }
        val incoming = Parcel.obtain()
        try {
            incoming.unmarshall(bytes, 0, bytes.size)
            incoming.setDataPosition(0)
            return checkNotNull(incoming.readBundle(javaClass.classLoader))
        } finally {
            incoming.recycle()
        }
    }
}
