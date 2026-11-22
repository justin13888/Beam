package dev.beam.android.ui

import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onFirst
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import coil3.ImageLoader
import coil3.SingletonImageLoader
import coil3.annotation.DelicateCoilApi
import coil3.serviceLoaderEnabled
import dagger.hilt.android.testing.HiltAndroidRule
import dagger.hilt.android.testing.HiltAndroidTest
import dagger.hilt.android.testing.HiltTestApplication
import dev.beam.android.MainActivity
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.Fixtures
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import javax.inject.Inject

/**
 * Opening a library lists that library's titles (issue #465).
 *
 * The real activity, navigation and view models, with the core replaced by
 * the fakes in `dev.beam.android.di`, so what is asserted is the query the
 * library's destination actually put on the catalog: its id, carried from the
 * back-stack entry into the screen's view model.
 */
@HiltAndroidTest
@RunWith(RobolectricTestRunner::class)
@Config(application = HiltTestApplication::class)
class BeamAppLibraryTest {
    @get:Rule(order = 0)
    val hilt = HiltAndroidRule(this)

    @get:Rule(order = 1)
    val compose = createEmptyComposeRule()

    @Inject
    lateinit var catalog: FakeCatalogRepository

    @Inject
    lateinit var playback: FakePlaybackRepository

    @OptIn(DelicateCoilApi::class)
    @Before
    fun setUp() {
        hilt.inject()
        // No fetchers, so every poster fails locally rather than reaching for
        // a server that does not exist.
        SingletonImageLoader.setUnsafe(
            ImageLoader.Builder(ApplicationProvider.getApplicationContext()).serviceLoaderEnabled(false).build(),
        )
        playback.continueWatchingRows = emptyList()
        catalog.libraryList =
            listOf(
                Fixtures.library(id = FILMS, name = "Cinema"),
                Fixtures.library(id = SERIES, name = "Television"),
            )
    }

    @OptIn(DelicateCoilApi::class)
    @After
    fun tearDown() = SingletonImageLoader.reset()

    @Test
    fun `opening a library browses only that library, and explore the whole catalog`() {
        ActivityScenario.launch(MainActivity::class.java).use {
            compose.onAllNodesWithText("Libraries").onFirst().performClick()
            compose.onNodeWithText("Television").performClick()
            compose.waitForIdle()

            val opened = catalog.browseCalls.last()
            assertEquals("the library's own id goes out with its browse", SERIES, opened.libraryId)
            assertTrue(
                "no library but the one opened is asked for",
                catalog.browseCalls.all { it.libraryId == null || it.libraryId == SERIES },
            )

            compose.onAllNodesWithText("Explore").onFirst().performClick()
            compose.waitForIdle()

            assertNull("the explore tab is the whole catalog", catalog.browseCalls.last().libraryId)
        }
    }

    private companion object {
        const val FILMS = "library-films"
        const val SERIES = "library-series"
    }
}
