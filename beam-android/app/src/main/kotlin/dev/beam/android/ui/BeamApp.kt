package dev.beam.android.ui

import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Download
import androidx.compose.material.icons.rounded.Home
import androidx.compose.material.icons.rounded.Search
import androidx.compose.material.icons.rounded.Settings
import androidx.compose.material.icons.rounded.VideoLibrary
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.adaptive.navigationsuite.NavigationSuiteScaffold
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavBackStack
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.feature.admin.AdminRoute
import dev.beam.android.feature.auth.AuthRoute
import dev.beam.android.feature.detail.DetailRoute
import dev.beam.android.feature.downloads.DownloadsRoute
import dev.beam.android.feature.explore.ExploreRoute
import dev.beam.android.feature.history.HistoryRoute
import dev.beam.android.feature.home.HomeRoute
import dev.beam.android.feature.libraries.LibrariesRoute
import dev.beam.android.feature.libraries.LibraryDetailRoute
import dev.beam.android.feature.player.PlayerRoute
import dev.beam.android.feature.settings.SettingsRoute
import dev.beam.android.navigation.Admin
import dev.beam.android.navigation.Destination
import dev.beam.android.navigation.History
import dev.beam.android.navigation.LibraryDetail
import dev.beam.android.navigation.MediaDetail
import dev.beam.android.navigation.Player
import dev.beam.android.navigation.SignIn
import dev.beam.android.navigation.TopLevel

/**
 * The whole app: one back stack, rendered by [NavDisplay].
 *
 * Navigation 3 makes the back stack ordinary observable state, which is what
 * lets the tab bar, the detail pane and the player share one history rather
 * than each keeping its own and disagreeing about what "back" means.
 */
@Composable
public fun BeamApp(
    isSignedIn: Boolean,
    isAdmin: Boolean,
    modifier: Modifier = Modifier,
) {
    val backStack = rememberNavBackStack(if (isSignedIn) TopLevel.Home else SignIn)
    var selectedTab by rememberSaveable { mutableStateOf(TopLevel.Home) }

    // The player and sign-in take the whole screen: a navigation bar over a
    // film, or beside a sign-in form, is chrome for a place the viewer cannot
    // currently go.
    val current = backStack.lastOrNull()
    val isImmersive = current is Player || current is SignIn

    if (isImmersive) {
        BeamNavDisplay(backStack, selectedTab, isAdmin, modifier.fillMaxSize()) { selectedTab = it }
        return
    }

    NavigationSuiteScaffold(
        modifier = modifier,
        navigationSuiteItems = {
            TopLevel.entries.forEach { destination ->
                item(
                    selected = selectedTab == destination,
                    onClick = {
                        selectedTab = destination
                        // Switching tabs resets to that tab's root rather than
                        // pushing on top of the current stack, which is what
                        // stops the back button walking backwards through
                        // every tab the viewer has ever touched.
                        backStack.clear()
                        backStack.add(destination)
                    },
                    icon = { Icon(destination.icon(), contentDescription = null) },
                    label = { Text(destination.label()) },
                )
            }
        },
    ) {
        BeamNavDisplay(backStack, selectedTab, isAdmin, Modifier.fillMaxSize()) {
            selectedTab = it
        }
    }
}

@Composable
private fun BeamNavDisplay(
    backStack: NavBackStack<NavKey>,
    selectedTab: TopLevel,
    isAdmin: Boolean,
    modifier: Modifier = Modifier,
    onSelectTab: (TopLevel) -> Unit,
) {
    NavDisplay(
        backStack = backStack,
        modifier = modifier,
        onBack = { backStack.removeLastOrNull() },
        entryDecorators =
            listOf(
                rememberSaveableStateHolderNavEntryDecorator(),
                // Without this a view model is rebuilt on every recomposition of
                // its entry, which for a screen holding a paged list means
                // refetching page one every time the viewer rotates the device.
                rememberViewModelStoreNavEntryDecorator(),
            ),
        entryProvider =
            entryProvider {
                entry<TopLevel> { destination ->
                    when (destination) {
                        TopLevel.Home -> {
                            HomeRoute(
                                onOpenMedia = { backStack.add(MediaDetail(it)) },
                                onResume = { entry ->
                                    backStack.add(
                                        Player(
                                            fileId = entry.fileId,
                                            mediaId = entry.media?.id,
                                            episodeId = entry.episode?.id,
                                            title = entry.media?.title.orEmpty(),
                                            startPositionSecs = entry.positionSecs,
                                        ),
                                    )
                                },
                            )
                        }

                        TopLevel.Libraries -> {
                            LibrariesRoute(
                                onOpenLibrary = { backStack.add(LibraryDetail(it)) },
                            )
                        }

                        TopLevel.Explore -> {
                            ExploreRoute(
                                onOpenMedia = { backStack.add(MediaDetail(it)) },
                            )
                        }

                        TopLevel.Downloads -> {
                            DownloadsRoute(
                                onPlay = { record ->
                                    backStack.add(
                                        Player(
                                            fileId = record.fileId,
                                            mediaId = record.mediaId,
                                            episodeId = record.episodeId,
                                            title = record.title,
                                        ),
                                    )
                                },
                            )
                        }

                        TopLevel.Settings -> {
                            SettingsRoute(
                                onSignedOut = {
                                    backStack.clear()
                                    backStack.add(SignIn)
                                },
                                onOpenHistory = { backStack.add(History) },
                                onOpenAdmin = { backStack.add(Admin) },
                            )
                        }
                    }
                }

                entry<MediaDetail> {
                    DetailRoute(
                        onPlay = { fileId, episodeId, title ->
                            backStack.add(
                                Player(
                                    fileId = fileId,
                                    mediaId = it.mediaId,
                                    episodeId = episodeId,
                                    title = title,
                                ),
                            )
                        },
                    )
                }

                entry<LibraryDetail> {
                    // The library's contents are the catalog filtered to it, so
                    // the explore screen renders them rather than a near-duplicate
                    // screen existing solely to show a different query.
                    ExploreRoute(onOpenMedia = { mediaId -> backStack.add(MediaDetail(mediaId)) })
                }

                entry<History> {
                    HistoryRoute(onOpenMedia = { backStack.add(MediaDetail(it)) })
                }

                entry<Admin> {
                    if (isAdmin) AdminRoute() else Box(Modifier.fillMaxSize())
                }

                entry<Player> { destination ->
                    PlayerRoute(
                        request =
                            PlaybackRequest(
                                mediaId = destination.mediaId.orEmpty(),
                                episodeId = destination.episodeId,
                                fileId = destination.fileId,
                                title = destination.title,
                                startPositionSecs = destination.startPositionSecs,
                            ),
                        onClose = { backStack.removeLastOrNull() },
                    )
                }

                entry<SignIn> {
                    AuthRoute(
                        onSignedIn = {
                            backStack.clear()
                            backStack.add(TopLevel.Home)
                            onSelectTab(TopLevel.Home)
                        },
                    )
                }
            },
    )
}

internal fun TopLevel.label(): String =
    when (this) {
        TopLevel.Home -> "Home"
        TopLevel.Libraries -> "Libraries"
        TopLevel.Explore -> "Explore"
        TopLevel.Downloads -> "Downloads"
        TopLevel.Settings -> "Settings"
    }

internal fun TopLevel.icon(): ImageVector =
    when (this) {
        TopLevel.Home -> Icons.Rounded.Home
        TopLevel.Libraries -> Icons.Rounded.VideoLibrary
        TopLevel.Explore -> Icons.Rounded.Search
        TopLevel.Downloads -> Icons.Rounded.Download
        TopLevel.Settings -> Icons.Rounded.Settings
    }
