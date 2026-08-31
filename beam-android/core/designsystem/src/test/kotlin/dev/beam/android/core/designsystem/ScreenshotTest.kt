package dev.beam.android.core.designsystem

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Surface
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.unit.dp
import com.github.takahirom.roborazzi.captureRoboImage
import dev.beam.android.core.designsystem.component.BeamEmptyState
import dev.beam.android.core.designsystem.component.BeamErrorState
import dev.beam.android.core.designsystem.component.MetaBadgeRow
import dev.beam.android.core.designsystem.component.SectionHeader
import dev.beam.android.core.designsystem.component.WatchProgressBar
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import org.robolectric.annotation.GraphicsMode

/**
 * The design system, rendered.
 *
 * These exist because the components they cover are the ones every screen
 * inherits: a regression in a badge or an empty state is a regression
 * everywhere at once, and it is exactly the kind of change that compiles
 * cleanly and reviews as harmless.
 *
 * Both themes are captured, because a colour defined only for light is
 * invisible on a dark background and nothing else would catch it.
 */
@RunWith(RobolectricTestRunner::class)
@GraphicsMode(GraphicsMode.Mode.NATIVE)
@Config(qualifiers = "w400dp-h800dp-mdpi")
class ScreenshotTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun `state views in light`() {
        capture("state-views-light", darkTheme = false) { StateViews() }
    }

    @Test
    fun `state views in dark`() {
        capture("state-views-dark", darkTheme = true) { StateViews() }
    }

    @Test
    fun `badges and progress in light`() {
        capture("badges-light", darkTheme = false) { Badges() }
    }

    @Test
    fun `badges and progress in dark`() {
        capture("badges-dark", darkTheme = true) { Badges() }
    }

    private fun capture(
        name: String,
        darkTheme: Boolean,
        content: @androidx.compose.runtime.Composable () -> Unit,
    ) {
        composeRule.setContent {
            // Dynamic colour is switched off deliberately: it derives the
            // palette from the wallpaper, which does not exist under
            // Robolectric and would make the reference images depend on the
            // host rather than on the code.
            BeamTheme(darkTheme = darkTheme, dynamicColor = false) {
                Surface { content() }
            }
        }
        composeRule.onRoot().captureRoboImage("src/test/screenshots/$name.png")
    }

    @androidx.compose.runtime.Composable
    private fun StateViews() {
        Column(
            modifier =
                Modifier
                    .fillMaxWidth()
                    .padding(BeamSpacing.Medium),
            verticalArrangement = Arrangement.spacedBy(BeamSpacing.Large),
        ) {
            SectionHeader(title = "Continue watching")
            BeamEmptyState(
                title = "Nothing here yet",
                description =
                    "Once your libraries have been scanned, what you can " +
                        "watch will appear here.",
            )
            BeamErrorState(
                message = "Could not reach the server.",
                onRetry = {},
            )
        }
    }

    @androidx.compose.runtime.Composable
    private fun Badges() {
        Column(
            modifier =
                Modifier
                    .fillMaxWidth()
                    .padding(BeamSpacing.Medium),
            verticalArrangement = Arrangement.spacedBy(BeamSpacing.Medium),
        ) {
            MetaBadgeRow(labels = listOf("2016", "1h 56m", "83%"))
            MetaBadgeRow(labels = listOf("4K", "HEVC", "HDR10", "5.1"))
            WatchProgressBar(progress = 0.0f, modifier = Modifier.width(200.dp))
            WatchProgressBar(progress = 0.42f, modifier = Modifier.width(200.dp))
            WatchProgressBar(progress = 1f, modifier = Modifier.width(200.dp))
        }
    }
}
