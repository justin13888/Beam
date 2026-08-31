package dev.beam.android.feature.settings

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Logout
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.designsystem.supportsDynamicColor
import dev.beam.android.core.model.PaletteSource
import dev.beam.android.core.model.QualityPreference
import dev.beam.android.core.model.ThemeMode
import java.text.DateFormat
import java.util.Date

/** Settings, wired to its view model. */
@Composable
public fun SettingsRoute(
    onSignedOut: () -> Unit,
    onOpenHistory: () -> Unit,
    onOpenAdmin: () -> Unit,
    modifier: Modifier = Modifier,
    viewModel: SettingsViewModel = hiltViewModel(),
) {
    val state by viewModel.state.collectAsStateWithLifecycle()

    LaunchedEffect(state.isSignedOut) {
        if (state.isSignedOut) onSignedOut()
    }

    SettingsScreen(
        state = state,
        onOpenHistory = onOpenHistory,
        onOpenAdmin = onOpenAdmin,
        onThemeMode = viewModel::setThemeMode,
        onPaletteSource = viewModel::setPaletteSource,
        onQuality = viewModel::setQuality,
        onAutoPlayNext = viewModel::setAutoPlayNext,
        onAllowSoftwareDecode = viewModel::setAllowSoftwareDecode,
        onDownloadOverCellular = viewModel::setDownloadOverCellular,
        onRevokeSession = viewModel::revokeSession,
        onForgetCertificates = viewModel::forgetCertificates,
        onSignOut = viewModel::signOut,
        onSignOutEverywhere = viewModel::signOutEverywhere,
        modifier = modifier,
    )
}

/** Settings, as a function of its state. */
@Composable
internal fun SettingsScreen(
    state: SettingsUiState,
    onOpenHistory: () -> Unit,
    onOpenAdmin: () -> Unit,
    onThemeMode: (ThemeMode) -> Unit,
    onPaletteSource: (PaletteSource) -> Unit,
    onQuality: (QualityPreference) -> Unit,
    onAutoPlayNext: (Boolean) -> Unit,
    onAllowSoftwareDecode: (Boolean) -> Unit,
    onDownloadOverCellular: (Boolean) -> Unit,
    onRevokeSession: (String) -> Unit,
    onForgetCertificates: () -> Unit,
    onSignOut: () -> Unit,
    onSignOutEverywhere: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var confirmingSignOutEverywhere by remember { mutableStateOf(false) }
    val preferences = state.preferences

    if (confirmingSignOutEverywhere) {
        AlertDialog(
            onDismissRequest = { confirmingSignOutEverywhere = false },
            title = { Text("Sign out everywhere?") },
            text = {
                Text(
                    "Every device signed in as you will be signed out, including this " +
                        "one. Downloads already on this device stay where they are.",
                )
            },
            confirmButton = {
                TextButton(
                    onClick = {
                        confirmingSignOutEverywhere = false
                        onSignOutEverywhere()
                    },
                ) { Text("Sign out everywhere") }
            },
            dismissButton = {
                TextButton(onClick = { confirmingSignOutEverywhere = false }) { Text("Cancel") }
            },
        )
    }

    LazyColumn(
        modifier = modifier.fillMaxSize(),
        contentPadding = PaddingValues(bottom = BeamSpacing.ExtraLarge),
    ) {
        state.user?.let { user ->
            item(key = "account-header") { SectionHeader(title = "Account") }
            item(key = "account") {
                ListItem(
                    headlineContent = { Text(user.displayName) },
                    supportingContent = { Text(user.email ?: state.server?.baseUrl.orEmpty()) },
                    trailingContent = {
                        IconButton(onClick = onSignOut) {
                            Icon(Icons.Rounded.Logout, contentDescription = "Sign out")
                        }
                    },
                )
            }
        }

        // History and the admin area live here rather than as their own tabs.
        // Neither is a browsing surface, and a five-tab bar that spent two of
        // its slots on screens most viewers open rarely would crowd out the
        // ones they open constantly.
        item(key = "history") {
            ListItem(
                headlineContent = { Text("Watch history") },
                supportingContent = { Text("Everything you have watched, newest first.") },
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .clickable(onClick = onOpenHistory),
            )
        }

        // Shown only to administrators. The server rejects these calls for
        // anyone else regardless, so hiding the row is a courtesy rather than
        // the control.
        if (state.user?.isAdmin == true) {
            item(key = "admin") {
                ListItem(
                    headlineContent = { Text("Administration") },
                    supportingContent = { Text("Libraries, users, and server status.") },
                    modifier =
                        Modifier
                            .fillMaxWidth()
                            .clickable(onClick = onOpenAdmin),
                )
            }
        }

        item(key = "appearance-header") { SectionHeader(title = "Appearance") }
        item(key = "theme") {
            ChoiceRow(
                title = "Theme",
                options = ThemeMode.entries,
                selected = preferences.themeMode,
                label = ThemeMode::label,
                onSelect = onThemeMode,
            )
        }
        if (supportsDynamicColor) {
            item(key = "palette") {
                ChoiceRow(
                    title = "Colour",
                    options = PaletteSource.entries,
                    selected = preferences.paletteSource,
                    label = PaletteSource::label,
                    onSelect = onPaletteSource,
                )
            }
        }

        item(key = "playback-header") { SectionHeader(title = "Playback") }
        item(key = "quality") {
            ChoiceRow(
                title = "Preferred quality",
                options = QualityPreference.entries,
                selected = preferences.quality,
                label = QualityPreference::label,
                onSelect = onQuality,
            )
        }
        item(key = "autoplay") {
            SwitchRow(
                title = "Play the next episode automatically",
                subtitle = null,
                checked = preferences.autoPlayNext,
                onChange = onAutoPlayNext,
            )
        }
        item(key = "software-decode") {
            SwitchRow(
                title = "Allow software decoding",
                // The honest description, not a reassuring one: Beam never
                // transcodes, so the alternative to software decoding is not
                // playing the file at all.
                subtitle =
                    "Plays files this device has no hardware decoder for. " +
                        "Expect stuttering and heavy battery use.",
                checked = preferences.allowSoftwareDecode,
                onChange = onAllowSoftwareDecode,
            )
        }

        item(key = "downloads-header") { SectionHeader(title = "Downloads") }
        item(key = "cellular") {
            SwitchRow(
                title = "Download without Wi-Fi",
                subtitle =
                    "Media files are large. Leaving this off avoids using your " +
                        "mobile data.",
                checked = preferences.downloadOverCellular,
                onChange = onDownloadOverCellular,
            )
        }

        if (state.sessions.isNotEmpty()) {
            item(key = "devices-header") { SectionHeader(title = "Signed-in devices") }
            items(state.sessions, key = { it.id }) { session ->
                ListItem(
                    headlineContent = { Text(session.ip) },
                    supportingContent = {
                        Text(
                            "Last active " +
                                DateFormat
                                    .getDateInstance()
                                    .format(Date(session.lastActiveUnix * MILLIS_PER_SECOND)),
                        )
                    },
                    trailingContent = {
                        TextButton(onClick = { onRevokeSession(session.id) }) {
                            Text("Revoke")
                        }
                    },
                )
            }
            item(key = "sign-out-everywhere") {
                ListItem(
                    headlineContent = { Text("Sign out everywhere") },
                    modifier = Modifier.clickable { confirmingSignOutEverywhere = true },
                )
            }
        }

        if (state.trustedCertificates.isNotEmpty()) {
            item(key = "certificates-header") { SectionHeader(title = "Trusted certificates") }
            items(state.trustedCertificates, key = { it }) { fingerprint ->
                ListItem(
                    headlineContent = {
                        Text(
                            text = fingerprint,
                            style =
                                MaterialTheme.typography.bodySmall.copy(
                                    fontFamily = FontFamily.Monospace,
                                ),
                        )
                    },
                )
            }
            item(key = "forget-certificates") {
                ListItem(
                    headlineContent = { Text("Forget these certificates") },
                    supportingContent = {
                        Text("You will be asked again the next time this server connects.")
                    },
                    modifier = Modifier.clickable(onClick = onForgetCertificates),
                )
            }
        }
    }
}

@Composable
private fun <T> ChoiceRow(
    title: String,
    options: List<T>,
    selected: T,
    label: (T) -> String,
    onSelect: (T) -> Unit,
) {
    // Cycles rather than opening a menu: every one of these has three options
    // at most, and a tap that changes the value immediately is less work than
    // a menu that has to be opened, read, and dismissed.
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = { Text(label(selected)) },
        modifier =
            Modifier
                .fillMaxWidth()
                .clickable {
                    val next = options[(options.indexOf(selected) + 1) % options.size]
                    onSelect(next)
                },
    )
}

@Composable
private fun SwitchRow(
    title: String,
    subtitle: String?,
    checked: Boolean,
    onChange: (Boolean) -> Unit,
) {
    ListItem(
        headlineContent = { Text(title) },
        supportingContent = subtitle?.let { { Text(it) } },
        trailingContent = { Switch(checked = checked, onCheckedChange = onChange) },
        modifier =
            Modifier
                .fillMaxWidth()
                .clickable { onChange(!checked) },
    )
}

internal fun ThemeMode.label(): String =
    when (this) {
        ThemeMode.System -> "Follow the system"
        ThemeMode.Light -> "Light"
        ThemeMode.Dark -> "Dark"
    }

internal fun PaletteSource.label(): String =
    when (this) {
        PaletteSource.Dynamic -> "From your wallpaper"
        PaletteSource.Brand -> "Beam's own colours"
    }

internal fun QualityPreference.label(): String =
    when (this) {
        QualityPreference.Best -> "Best this device can play"
        QualityPreference.MatchScreen -> "Match this screen"
        QualityPreference.Smallest -> "Smallest file"
    }

private const val MILLIS_PER_SECOND = 1_000L
