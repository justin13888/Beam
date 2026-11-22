package dev.beam.android.feature.admin

import androidx.navigation3.runtime.NavKey
import dev.beam.android.core.testing.AdminFixtures
import dev.beam.android.core.testing.FakeAdminRepository
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.EnrichmentStatus

/** Moving through the admin area's pages, below the app's one admin destination. */
@OptIn(ExperimentalCoroutinesApi::class)
class AdminNavigationTest {
    private val backStack: MutableList<NavKey> = mutableListOf(AdminPage.Dashboard)
    private var titlesClosed = 0
    private val navigation = AdminNavigation(backStack, onTitleClosed = { titlesClosed++ })

    @Before
    fun setUp() = Dispatchers.setMain(StandardTestDispatcher())

    @After
    fun tearDown() = Dispatchers.resetMain()

    @Test
    fun `each page opens over the one it was opened from and closes back to it`() {
        for (page in listOf(AdminPage.Logs, AdminPage.Events, AdminPage.Metadata, AdminPage.LibraryTelemetry)) {
            navigation.open(page)
            assertEquals(listOf(AdminPage.Dashboard, page), backStack)
            navigation.back()
            assertEquals(listOf<NavKey>(AdminPage.Dashboard), backStack)
        }
        assertEquals("no Title page was closed", 0, titlesClosed)
    }

    @Test
    fun `closing a title opened from the metadata list is reported, and closing the list is not`() {
        navigation.open(AdminPage.Metadata)
        navigation.open(AdminPage.Title("dark", "Dark"))

        navigation.back()
        assertEquals(listOf(AdminPage.Dashboard, AdminPage.Metadata), backStack)
        assertEquals(1, titlesClosed)

        navigation.open(AdminPage.Title("heat", "Heat"))
        navigation.back()
        assertEquals(2, titlesClosed)

        navigation.back()
        assertEquals(listOf<NavKey>(AdminPage.Dashboard), backStack)
        assertEquals(2, titlesClosed)
    }

    @Test
    fun `back on the dashboard leaves it in place`() {
        navigation.back()
        assertEquals(listOf<NavKey>(AdminPage.Dashboard), backStack)
    }

    @Test
    fun `the metadata list reads again on returning from a title, and not when it is rebuilt`() =
        runTest {
            val admin =
                FakeAdminRepository().apply {
                    enrichments += AdminFixtures.enrichment(mediaId = "dark", title = "Dark")
                }
            val list = MetadataListViewModel(admin)
            navigation.open(AdminPage.Metadata)
            list.titlesClosed(titlesClosed)
            testScheduler.advanceUntilIdle()
            assertEquals(
                listOf("dark"),
                list.state.value.items
                    .map { it.mediaId },
            )

            // A fix-match on the title moves it out of the unmatched list.
            navigation.open(AdminPage.Title("dark", "Dark"))
            admin.enrichments[0] = admin.enrichments[0].copy(status = EnrichmentStatus.PENDING)

            // A rotation rebuilds the list with the count it already saw.
            list.titlesClosed(titlesClosed)
            testScheduler.advanceUntilIdle()
            assertEquals(
                "the pages already loaded are kept",
                listOf("dark"),
                list.state.value.items
                    .map { it.mediaId },
            )

            navigation.back()
            list.titlesClosed(titlesClosed)
            testScheduler.advanceUntilIdle()
            assertEquals(
                emptyList<String>(),
                list.state.value.items
                    .map { it.mediaId },
            )
        }
}
