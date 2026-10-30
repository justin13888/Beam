package dev.beam.android.widget

import android.app.Application
import androidx.glance.appwidget.testing.unit.runGlanceAppWidgetUnitTest
import androidx.glance.testing.unit.hasClickAction
import androidx.glance.testing.unit.hasText
import dev.beam.android.core.testing.Fixtures
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * The home-screen widget's content, rendered by Glance's unit-test host.
 *
 * Robolectric for the context Glance renders against; a plain [Application]
 * keeps the app's own, which loads the native core, out of the test.
 */
@RunWith(RobolectricTestRunner::class)
@Config(application = Application::class)
class ContinueWatchingWidgetTest {
    @Test
    fun `the widget lists continue watching rows`() =
        runGlanceAppWidgetUnitTest {
            val rows =
                listOf(
                    Fixtures.continueWatching(mediaId = "movie-1", media = Fixtures.movie(title = "Le Samourai")),
                    // An episode carries its show as the media; the show's
                    // title is what the row names.
                    Fixtures.continueWatching(
                        mediaId = "show-1",
                        media = Fixtures.show(title = "Le Bureau"),
                        episode = Fixtures.episode(title = "Pilot"),
                    ),
                    // Neither half known: the row still says something.
                    Fixtures.continueWatching(mediaId = "gone", media = null),
                )

            provideComposable { ContinueWatchingWidget().WidgetContent(rows) }

            // Every text node, in the order the widget lays them out: the
            // heading, then one title per row in the order the core ranked.
            val texts = onAllNodes(hasText(""))
            texts.assertCountEquals(4)
            listOf("Continue watching", "Le Samourai", "Le Bureau", "Unknown title").forEachIndexed { index, text ->
                texts[index].assert(hasText(text))
            }
            // Each row, and only a row, opens the app.
            onAllNodes(hasClickAction()).assertCountEquals(3)
        }

    @Test
    fun `an empty list says so rather than leaving the widget blank`() =
        runGlanceAppWidgetUnitTest {
            provideComposable { ContinueWatchingWidget().WidgetContent(emptyList()) }

            onNode(hasText("Nothing in progress")).assertExists()
            onAllNodes(hasClickAction()).assertCountEquals(0)
        }
}
