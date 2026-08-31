package dev.beam.android.core.designsystem.component

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.CloudOff
import androidx.compose.material.icons.rounded.Inbox
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.semantics.LiveRegionMode
import androidx.compose.ui.semantics.liveRegion
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.beam.android.core.designsystem.BeamSpacing
import dev.beam.android.core.designsystem.BeamTextStyles

/** A centred spinner for a screen with nothing to show yet. */
@Composable
public fun BeamLoading(modifier: Modifier = Modifier) {
    Column(
        modifier = modifier.fillMaxSize(),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.Center,
    ) {
        CircularProgressIndicator()
    }
}

/**
 * A screen with nothing in it, and a reason why.
 *
 * Always takes a reason: "No results" alone leaves the user unsure whether
 * their filter is wrong or the library is empty.
 */
@Composable
public fun BeamEmptyState(
    title: String,
    description: String,
    modifier: Modifier = Modifier,
    icon: ImageVector = Icons.Rounded.Inbox,
    actionLabel: String? = null,
    onAction: (() -> Unit)? = null,
) {
    StateScaffold(
        icon = icon,
        title = title,
        description = description,
        modifier = modifier,
        actionLabel = actionLabel,
        onAction = onAction,
    )
}

/**
 * A failure, with a retry when retrying could plausibly help.
 *
 * @param onRetry omitted for errors that will not fix themselves, so the UI
 *   never offers a button that cannot work.
 */
@Composable
public fun BeamErrorState(
    message: String,
    modifier: Modifier = Modifier,
    onRetry: (() -> Unit)? = null,
) {
    StateScaffold(
        icon = Icons.Rounded.CloudOff,
        title = "Something went wrong",
        description = message,
        modifier = modifier,
        actionLabel = if (onRetry != null) "Try again" else null,
        onAction = onRetry,
        // Announced when it replaces content, so a screen reader user learns
        // the load failed instead of hearing silence.
        announce = true,
    )
}

@Composable
private fun StateScaffold(
    icon: ImageVector,
    title: String,
    description: String,
    modifier: Modifier,
    actionLabel: String?,
    onAction: (() -> Unit)?,
    announce: Boolean = false,
) {
    Column(
        modifier =
            modifier
                .fillMaxWidth()
                .padding(BeamSpacing.ExtraLarge)
                .then(
                    if (announce) {
                        Modifier.semantics { liveRegion = LiveRegionMode.Polite }
                    } else {
                        Modifier
                    },
                ),
        horizontalAlignment = Alignment.CenterHorizontally,
        verticalArrangement = Arrangement.spacedBy(BeamSpacing.Compact, Alignment.CenterVertically),
    ) {
        Icon(
            imageVector = icon,
            contentDescription = null,
            modifier = Modifier.size(48.dp),
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(text = title, style = MaterialTheme.typography.titleMedium)
        Text(
            text = description,
            style = BeamTextStyles.EmptyStateBody,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        if (actionLabel != null && onAction != null) {
            Button(onClick = onAction) { Text(actionLabel) }
        }
    }
}
