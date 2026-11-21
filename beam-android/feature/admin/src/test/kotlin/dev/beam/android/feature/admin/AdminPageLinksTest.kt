package dev.beam.android.feature.admin

import androidx.compose.ui.test.hasScrollAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollToNode
import dev.beam.android.core.model.LoadState
import dev.beam.android.core.testing.AdminFixtures
import dev.beam.android.core.testing.Fixtures
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

/** The rows that open the admin area's pages, as an operator taps them. */
@RunWith(RobolectricTestRunner::class)
@Config(qualifiers = "w400dp-h800dp-mdpi")
class AdminPageLinksTest {
    @get:Rule
    val composeRule = createComposeRule()

    @Test
    fun `each dashboard row opens its page`() {
        val opened = mutableListOf<AdminPage>()
        composeRule.setContent {
            AdminScreen(
                state = LoadState.Success(AdminUiState(status = Fixtures.adminStatus())),
                onScan = {},
                onDeleteLibrary = {},
                onSetUserDisabled = { _, _ -> },
                onRetry = {},
                onOpen = { opened += it },
            )
        }

        val rows =
            listOf(
                "Fix matches and lock fields" to AdminPage.Metadata,
                "Logs" to AdminPage.Logs,
                "Events" to AdminPage.Events,
                "Library telemetry" to AdminPage.LibraryTelemetry,
            )
        for ((label, _) in rows) {
            composeRule.onNode(hasScrollAction()).performScrollToNode(hasText(label))
            composeRule.onNodeWithText(label).performClick()
        }

        assertEquals(rows.map { it.second }, opened)
    }

    @Test
    fun `a title in the metadata list opens that title's page`() {
        val opened = mutableListOf<AdminPage>()
        composeRule.setContent {
            MetadataListScreen(
                state =
                    PagedUiState(
                        items =
                            listOf(
                                AdminFixtures.enrichment(mediaId = "dark", title = "Dark"),
                                AdminFixtures.enrichment(mediaId = "heat", title = "Heat"),
                            ),
                        total = 2uL,
                    ),
                status = null,
                onStatus = {},
                onBack = {},
                onRefresh = {},
                onLoadMore = {},
                onOpenTitle = { id, title -> opened += AdminPage.Title(id, title) },
            )
        }

        // The row reads "Heat (year)"; the title's page is named by the title alone.
        composeRule.onNodeWithText("Heat", substring = true).performClick()

        assertEquals(listOf<AdminPage>(AdminPage.Title("heat", "Heat")), opened)
    }
}
