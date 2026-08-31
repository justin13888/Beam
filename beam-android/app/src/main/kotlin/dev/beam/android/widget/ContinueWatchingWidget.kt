package dev.beam.android.widget

import android.content.Context
import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.dp
import androidx.glance.GlanceId
import androidx.glance.GlanceModifier
import androidx.glance.GlanceTheme
import androidx.glance.Image
import androidx.glance.ImageProvider
import androidx.glance.action.actionStartActivity
import androidx.glance.action.clickable
import androidx.glance.appwidget.GlanceAppWidget
import androidx.glance.appwidget.GlanceAppWidgetReceiver
import androidx.glance.appwidget.SizeMode
import androidx.glance.appwidget.cornerRadius
import androidx.glance.appwidget.provideContent
import androidx.glance.background
import androidx.glance.layout.Alignment
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.padding
import androidx.glance.layout.size
import androidx.glance.text.Text
import androidx.glance.text.TextStyle
import dagger.hilt.EntryPoint
import dagger.hilt.InstallIn
import dagger.hilt.android.EntryPointAccessors
import dagger.hilt.components.SingletonComponent
import dev.beam.android.MainActivity
import dev.beam.android.core.ffi.repository.PlaybackRepository
import uniffi.beam_client_core.ContinueWatchingEntry

/**
 * A home-screen widget for picking up where the viewer left off.
 *
 * The single highest-value thing a media app can put on a home screen: the
 * common case is resuming one specific thing, and a widget removes the app
 * launch, the home screen, and the scroll from that.
 */
internal class ContinueWatchingWidget : GlanceAppWidget() {
    // Responsive rather than fixed: a 2x1 widget shows one title, a wider one
    // shows several, and Glance picks per placement rather than making the
    // viewer choose a size that fits the content.
    override val sizeMode: SizeMode = SizeMode.Exact

    override suspend fun provideGlance(
        context: Context,
        id: GlanceId,
    ) {
        val repository =
            EntryPointAccessors
                .fromApplication(context, WidgetEntryPoint::class.java)
                .playbackRepository()

        // Fetched before `provideContent` rather than inside it: a widget's
        // composition has no lifecycle to launch a coroutine from, and a
        // failure here must still render something rather than leaving the
        // widget blank on the home screen forever.
        val entries =
            runCatching { repository.continueWatching(WIDGET_LIMIT) }
                .getOrDefault(emptyList())

        provideContent {
            GlanceTheme {
                WidgetContent(entries)
            }
        }
    }

    @Composable
    private fun WidgetContent(entries: List<ContinueWatchingEntry>) {
        Column(
            modifier =
                GlanceModifier
                    .fillMaxSize()
                    .background(GlanceTheme.colors.widgetBackground)
                    .cornerRadius(16.dp)
                    .padding(12.dp),
            verticalAlignment = Alignment.Top,
        ) {
            Text(
                text = "Continue watching",
                style = TextStyle(color = GlanceTheme.colors.onSurfaceVariant),
            )

            if (entries.isEmpty()) {
                Text(
                    text = "Nothing in progress",
                    style = TextStyle(color = GlanceTheme.colors.onSurface),
                    modifier = GlanceModifier.padding(top = 8.dp),
                )
                return@Column
            }

            entries.forEach { entry ->
                Row(
                    modifier =
                        GlanceModifier
                            .fillMaxWidth()
                            .padding(top = 8.dp)
                            .clickable(actionStartActivity(MainActivity::class.java)),
                    verticalAlignment = Alignment.CenterVertically,
                ) {
                    Text(
                        text = entry.media?.title ?: entry.episode?.title ?: "Unknown title",
                        style = TextStyle(color = GlanceTheme.colors.onSurface),
                        maxLines = 1,
                    )
                }
            }
        }
    }

    private companion object {
        const val WIDGET_LIMIT: UInt = 3u
    }
}

/** The receiver the platform instantiates. */
internal class ContinueWatchingWidgetReceiver : GlanceAppWidgetReceiver() {
    override val glanceAppWidget: GlanceAppWidget = ContinueWatchingWidget()
}

/**
 * How the widget reaches the graph.
 *
 * A widget is created by the platform, not by Hilt, so it cannot be an
 * `@AndroidEntryPoint`; an entry point is the supported way in.
 */
@EntryPoint
@InstallIn(SingletonComponent::class)
internal interface WidgetEntryPoint {
    fun playbackRepository(): PlaybackRepository
}
