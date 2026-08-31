package dev.beam.android.navigation

import androidx.navigation3.runtime.NavKey
import kotlinx.serialization.Serializable

/**
 * Every place the app can be.
 *
 * Navigation 3 keys rather than string routes: the destination *is* the data,
 * so an argument cannot be spelled wrong, forgotten, or lost to URL encoding,
 * and the compiler checks that a navigation call carries what the screen needs.
 */
public sealed interface Destination : NavKey

/** The top-level tabs, which the navigation bar and rail render. */
@Serializable
public enum class TopLevel : Destination {
    /** Continue watching and the curated rows. */
    Home,

    /** Every library on the server. */
    Libraries,

    /** The whole catalog, filtered and searched. */
    Explore,

    /** Offline downloads. */
    Downloads,

    /** Preferences, the account, and this device's trust decisions. */
    Settings,
}

/** One title's page. */
@Serializable
public data class MediaDetail(
    val mediaId: String,
) : Destination

/** One library's contents. */
@Serializable
public data class LibraryDetail(
    val libraryId: String,
) : Destination

/** Watch history. */
@Serializable
public data object History : Destination

/** The administrative area. */
@Serializable
public data object Admin : Destination

/**
 * Fullscreen playback.
 *
 * Its own destination rather than an overlay, which is what makes rotation,
 * picture-in-picture and the back stack behave: the player is a place the
 * viewer navigated to, and the system treats it as one.
 */
@Serializable
public data class Player(
    val fileId: String,
    val mediaId: String? = null,
    val episodeId: String? = null,
    val title: String = "",
    val startPositionSecs: Double = 0.0,
) : Destination

/** Sign-in, shown when there is no session. */
@Serializable
public data object SignIn : Destination
