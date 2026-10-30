package dev.beam.android.ui

import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onFirst
import androidx.compose.ui.test.onNodeWithContentDescription
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
import dev.beam.android.core.testing.FakeServerRepository
import dev.beam.android.core.testing.Fixtures
import kotlinx.coroutines.runBlocking
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.beam_client_core.SessionState
import javax.inject.Inject

/**
 * Leaving a session leaves nothing reached with it on the back stack.
 *
 * The real activity, view models and navigation, with the core replaced by the
 * fakes in `dev.beam.android.di`. The back stack is asserted by what the
 * viewer can do with it: sign-in is showing, and back leaves the app. Had
 * anything stayed under sign-in, back would return to it instead.
 */
@HiltAndroidTest
@RunWith(RobolectricTestRunner::class)
@Config(application = HiltTestApplication::class)
class BeamAppBackStackTest {
    @get:Rule(order = 0)
    val hilt = HiltAndroidRule(this)

    @get:Rule(order = 1)
    val compose = createEmptyComposeRule()

    @Inject
    lateinit var servers: FakeServerRepository

    @Inject
    lateinit var catalog: FakeCatalogRepository

    @Inject
    lateinit var playback: FakePlaybackRepository

    @OptIn(DelicateCoilApi::class)
    @Before
    fun setUp() {
        hilt.inject()
        // Every poster is a URL on a server that does not exist. A loader with
        // no fetchers registered fails each one locally instead of reaching
        // for the network, which these tests must never touch.
        SingletonImageLoader.setUnsafe(
            ImageLoader.Builder(ApplicationProvider.getApplicationContext()).serviceLoaderEnabled(false).build(),
        )
        // Home's resume row plays rather than opening a title; without it the
        // only way into a title from home is the one these tests take.
        playback.continueWatchingRows = emptyList()
    }

    @OptIn(DelicateCoilApi::class)
    @After
    fun tearDown() = SingletonImageLoader.reset()

    @Test
    fun `signing out resets the back stack to sign in`() {
        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            compose.onNodeWithText("Settings").performClick()
            compose.onNodeWithContentDescription("Sign out").performClick()

            assertSignInIsTheWholeBackStack(scenario)
            assertEquals(SessionState.LoggedOut, runBlocking { servers.activeServer() }?.state)
        }
    }

    @Test
    fun `a session expiring mid-use returns to sign in`() {
        val movie = Fixtures.movie()
        catalog.details[movie.id] = Fixtures.movieDetail(movie)

        ActivityScenario.launch(MainActivity::class.java).use { scenario ->
            compose.onAllNodesWithText(movie.title).onFirst().performClick()
            // The title's own page, loaded under the session about to go.
            compose.onNodeWithContentDescription("Mark watched").assertExists()

            servers.expire(Fixtures.server().id)

            assertSignInIsTheWholeBackStack(scenario)
        }
    }

    private fun assertSignInIsTheWholeBackStack(scenario: ActivityScenario<MainActivity>) {
        compose.waitForIdle()
        compose.onNodeWithText("Server address").assertExists()
        scenario.onActivity { activity ->
            activity.onBackPressedDispatcher.onBackPressed()
            assertTrue("back from sign-in leaves the app", activity.isFinishing)
        }
    }
}
