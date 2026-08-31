package dev.beam.android.core.designsystem.component

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.rounded.Movie
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.semantics.clearAndSetSemantics
import coil3.compose.AsyncImage
import dev.beam.android.core.designsystem.BeamShapeDefaults
import dev.beam.android.core.designsystem.BeamSizes

/**
 * A poster, thumbnail or backdrop.
 *
 * Artwork is decorative whenever the title it belongs to is already written
 * beside it, which is the usual case in a catalog. Passing a null
 * [contentDescription] removes it from the accessibility tree entirely rather
 * than making a screen reader announce every tile's image before its label.
 *
 * @param url absolute artwork URL, or null when the title has none.
 * @param aspectRatio width divided by height; posters are 2:3, stills 16:9.
 * @param contentDescription spoken description, or null when decorative.
 * @param fallbackIcon shown when there is no artwork or it fails to load.
 */
@Composable
public fun Artwork(
    url: String?,
    modifier: Modifier = Modifier,
    aspectRatio: Float = BeamSizes.PosterAspectRatio,
    contentDescription: String? = null,
    fallbackIcon: ImageVector = Icons.Rounded.Movie,
) {
    var state by remember(url) { mutableStateOf(ArtworkState.Loading) }

    Box(
        modifier =
            modifier
                .fillMaxWidth()
                .aspectRatio(aspectRatio)
                .clip(BeamShapeDefaults.Artwork)
                .background(MaterialTheme.colorScheme.surfaceContainerHighest)
                .then(
                    if (contentDescription == null) {
                        Modifier.clearAndSetSemantics {}
                    } else {
                        Modifier
                    },
                ),
        contentAlignment = Alignment.Center,
    ) {
        if (state == ArtworkState.Loading) {
            Box(Modifier.fillMaxSize().shimmer())
        }
        if (url != null && state != ArtworkState.Failed) {
            AsyncImage(
                model = url,
                contentDescription = contentDescription,
                modifier = Modifier.fillMaxSize(),
                contentScale = ContentScale.Crop,
                onSuccess = { state = ArtworkState.Loaded },
                onError = { state = ArtworkState.Failed },
            )
        } else if (url == null || state == ArtworkState.Failed) {
            Icon(
                imageVector = fallbackIcon,
                contentDescription = null,
                tint = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

private enum class ArtworkState { Loading, Loaded, Failed }
