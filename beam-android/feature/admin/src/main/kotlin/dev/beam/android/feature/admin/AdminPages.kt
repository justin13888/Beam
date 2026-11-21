package dev.beam.android.feature.admin

import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.rounded.ArrowBack
import androidx.compose.material.icons.rounded.Refresh
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.navigation3.rememberViewModelStoreNavEntryDecorator
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.runtime.rememberSaveableStateHolderNavEntryDecorator
import androidx.navigation3.ui.NavDisplay
import dev.beam.android.core.designsystem.BeamSpacing
import kotlinx.serialization.Serializable

/** The pages of the admin area, below the dashboard. */
@Serializable
internal sealed interface AdminPage : NavKey {
    /** The dashboard: counts, libraries, users, playback telemetry. */
    @Serializable
    data object Dashboard : AdminPage

    /** The operational log, paged. */
    @Serializable
    data object Logs : AdminPage

    /** The server's recent events. */
    @Serializable
    data object Events : AdminPage

    /** Titles by enrichment status, the unmatched list first. */
    @Serializable
    data object Metadata : AdminPage

    /** One title's fix-match and field locks. */
    @Serializable
    data class Title(
        val mediaId: String,
        val title: String,
    ) : AdminPage

    /** The library telemetry report the server would send. */
    @Serializable
    data object LibraryTelemetry : AdminPage
}

/**
 * The admin area, wired to its view models.
 *
 * Its pages are a back stack of their own, inside the app's one admin
 * destination: the app's navigation knows only that there is an admin area,
 * and each page's view model lives exactly as long as the page does.
 */
@Composable
public fun AdminRoute(modifier: Modifier = Modifier) {
    val backStack = rememberNavBackStack(AdminPage.Dashboard)
    val open: (AdminPage) -> Unit = { backStack.add(it) }
    val back: () -> Unit = { if (backStack.size > 1) backStack.removeLastOrNull() }
    NavDisplay(
        backStack = backStack,
        modifier = modifier,
        onBack = back,
        entryDecorators =
            listOf(
                rememberSaveableStateHolderNavEntryDecorator(),
                rememberViewModelStoreNavEntryDecorator(),
            ),
        entryProvider =
            entryProvider {
                entry<AdminPage.Dashboard> { DashboardRoute(onOpen = open) }
                entry<AdminPage.Logs> { LogsRoute(onBack = back) }
                entry<AdminPage.Events> { EventsRoute(onBack = back) }
                entry<AdminPage.Metadata> {
                    MetadataListRoute(onBack = back, onOpenTitle = { id, title -> open(AdminPage.Title(id, title)) })
                }
                entry<AdminPage.Title> { page -> TitleMetadataRoute(page.mediaId, page.title, onBack = back) }
                entry<AdminPage.LibraryTelemetry> { LibraryTelemetryRoute(onBack = back) }
            },
    )
}

@Composable
private fun DashboardRoute(onOpen: (AdminPage) -> Unit) {
    val viewModel: AdminViewModel = hiltViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    AdminScreen(
        state = state,
        onScan = viewModel::scan,
        onDeleteLibrary = viewModel::deleteLibrary,
        onSetUserDisabled = viewModel::setUserDisabled,
        onTelemetryRange = viewModel::setTelemetryRange,
        onRetry = viewModel::refresh,
        onCreateLibrary = viewModel::createLibrary,
        onMessageShown = viewModel::clearMessage,
        onOpen = onOpen,
    )
}

@Composable
private fun LogsRoute(onBack: () -> Unit) {
    val viewModel: AdminLogsViewModel = hiltViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    LogsScreen(state = state, onBack = onBack, onRefresh = viewModel::refresh, onLoadMore = viewModel::loadMore)
}

@Composable
private fun EventsRoute(onBack: () -> Unit) {
    val viewModel: AdminEventsViewModel = hiltViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    EventsScreen(state = state, onBack = onBack, onRefresh = viewModel::refresh)
}

@Composable
private fun MetadataListRoute(
    onBack: () -> Unit,
    onOpenTitle: (mediaId: String, title: String) -> Unit,
) {
    val viewModel: MetadataListViewModel = hiltViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    val status by viewModel.status.collectAsStateWithLifecycle()
    // The list leaves composition while a title is open over it, and a fix-match
    // there moves the title out of the unmatched list; read it again on return.
    // The first showing is the view model's own first read.
    var shown by rememberSaveable { mutableStateOf(false) }
    LaunchedEffect(Unit) { if (shown) viewModel.refresh() else shown = true }
    MetadataListScreen(
        state = state,
        status = status,
        onStatus = viewModel::setStatus,
        onBack = onBack,
        onRefresh = viewModel::refresh,
        onLoadMore = viewModel::loadMore,
        onOpenTitle = onOpenTitle,
    )
}

@Composable
private fun TitleMetadataRoute(
    mediaId: String,
    title: String,
    onBack: () -> Unit,
) {
    val viewModel: TitleMetadataViewModel = hiltViewModel()
    LaunchedEffect(mediaId) { viewModel.open(mediaId) }
    val state by viewModel.state.collectAsStateWithLifecycle()
    TitleMetadataScreen(
        title = title,
        state = state,
        onBack = onBack,
        onSearch = viewModel::search,
        onFixMatch = viewModel::fixMatch,
        onClearMatch = viewModel::clearMatch,
        onToggleLock = viewModel::toggleLock,
        onSaveLocks = viewModel::saveLocks,
        onMessageShown = viewModel::clearMessage,
    )
}

@Composable
private fun LibraryTelemetryRoute(onBack: () -> Unit) {
    val viewModel: LibraryTelemetryViewModel = hiltViewModel()
    val state by viewModel.state.collectAsStateWithLifecycle()
    LibraryTelemetryScreen(state = state, onBack = onBack, onRefresh = viewModel::refresh)
}

/** A page's heading: the way back, its title, and a refresh where it has one. */
@Composable
internal fun PageHeader(
    title: String,
    onBack: () -> Unit,
    onRefresh: (() -> Unit)? = null,
) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(end = BeamSpacing.Small),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        IconButton(onClick = onBack) {
            Icon(Icons.AutoMirrored.Rounded.ArrowBack, contentDescription = "Back")
        }
        Text(
            text = title,
            style = MaterialTheme.typography.titleLarge,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            modifier = Modifier.weight(1f),
        )
        if (onRefresh != null) {
            IconButton(onClick = onRefresh) {
                Icon(Icons.Rounded.Refresh, contentDescription = "Refresh")
            }
        }
    }
}
