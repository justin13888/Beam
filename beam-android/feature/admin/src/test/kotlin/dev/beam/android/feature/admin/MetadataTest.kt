package dev.beam.android.feature.admin

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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.EnrichmentStatus
import uniffi.beam_client_core.MediaKind
import uniffi.beam_client_core.MetadataField
import uniffi.beam_client_core.PinSource

/** The unmatched list, fix-match and field locks. */
@OptIn(ExperimentalCoroutinesApi::class)
class MetadataTest {
    private val dispatcher = StandardTestDispatcher()

    @Before
    fun setUp() = Dispatchers.setMain(dispatcher)

    @After
    fun tearDown() = Dispatchers.resetMain()

    private fun server(): FakeAdminRepository =
        FakeAdminRepository().apply {
            enrichments +=
                listOf(
                    AdminFixtures.enrichment(mediaId = "dark", title = "Dark"),
                    AdminFixtures.enrichment(
                        mediaId = "heat",
                        title = "Heat",
                        kind = MediaKind.MOVIE,
                        status = EnrichmentStatus.ENRICHED,
                    ),
                    AdminFixtures.enrichment(mediaId = "lost", title = "Lost", status = EnrichmentStatus.FAILED),
                )
        }

    @Test
    fun `the metadata list opens on the unmatched titles and filters by the status picked`() =
        runTest {
            val viewModel = MetadataListViewModel(server())
            testScheduler.advanceUntilIdle()
            assertEquals(EnrichmentStatus.UNMATCHED, viewModel.status.value)
            assertEquals(
                listOf("dark"),
                viewModel.state.value.items
                    .map { it.mediaId },
            )

            viewModel.setStatus(EnrichmentStatus.FAILED)
            testScheduler.advanceUntilIdle()
            assertEquals(
                listOf("lost"),
                viewModel.state.value.items
                    .map { it.mediaId },
            )

            viewModel.setStatus(null)
            testScheduler.advanceUntilIdle()
            assertEquals(
                listOf("dark", "heat", "lost"),
                viewModel.state.value.items
                    .map { it.mediaId },
            )
            assertEquals(3uL, viewModel.state.value.total)
        }

    @Test
    fun `fixing a match searches, then pins the title to the candidate picked`() =
        runTest {
            val admin = server()
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()
            assertEquals(
                EnrichmentStatus.UNMATCHED,
                viewModel.state.value.enrichment
                    ?.status,
            )

            viewModel.search("  dark netflix ", 2017u)
            testScheduler.advanceUntilIdle()
            assertEquals(listOf(Triple("dark", "dark netflix", 2017u)), admin.candidateSearches)
            val candidate =
                viewModel.state.value.candidates!!
                    .single()

            viewModel.fixMatch(candidate)
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            val pinned = admin.enrichments.first { it.mediaId == "dark" }
            assertEquals(candidate.externalRef, pinned.pinnedRef)
            assertEquals("the screen shows the title as the server answered it", pinned, state.enrichment)
            assertEquals(EnrichmentStatus.PENDING, state.enrichment?.status)
            assertFalse(state.isSaving)
            assertTrue(state.message!!.startsWith("Matched"))
        }

    @Test
    fun `a blank search asks for the title's own name rather than for nothing`() =
        runTest {
            val admin = server()
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()

            viewModel.search("   ", null)
            testScheduler.advanceUntilIdle()

            assertEquals(listOf(Triple("dark", null, null)), admin.candidateSearches)
        }

    @Test
    fun `a search on a server with no provider says there is nothing to search`() =
        runTest {
            val admin =
                server().apply {
                    matchingFailWith = BeamException.Server(409u, false, "not configured", "about:blank")
                }
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()

            viewModel.search("", null)
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertNull(state.candidates)
            assertTrue(state.searchError!!.contains("No metadata provider"))
            assertFalse(state.isSearching)
        }

    @Test
    fun `clearing a fixed match unpins the title`() =
        runTest {
            val admin =
                FakeAdminRepository().apply {
                    enrichments +=
                        AdminFixtures.enrichment(mediaId = "dark", pinnedRef = "tmdb:1", pinSource = PinSource.ADMIN)
                }
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()

            viewModel.clearMatch()
            testScheduler.advanceUntilIdle()

            assertNull(admin.enrichments.single().pinnedRef)
            assertNull(
                viewModel.state.value.enrichment
                    ?.pinnedRef,
            )
            assertEquals("Fixed match cleared.", viewModel.state.value.message)
        }

    @Test
    fun `ticked locks are sent whole on save, and only then`() =
        runTest {
            val admin =
                FakeAdminRepository().apply {
                    enrichments +=
                        AdminFixtures.enrichment(mediaId = "dark", lockedFields = listOf(MetadataField.POSTER))
                }
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()
            assertEquals(setOf(MetadataField.POSTER), viewModel.state.value.locks)

            viewModel.toggleLock(MetadataField.GENRES)
            viewModel.toggleLock(MetadataField.POSTER)
            testScheduler.advanceUntilIdle()
            assertTrue(viewModel.state.value.hasUnsavedLocks)
            assertEquals(
                "nothing is sent before save",
                listOf(MetadataField.POSTER),
                admin.enrichments.single().lockedFields,
            )

            viewModel.saveLocks()
            testScheduler.advanceUntilIdle()

            assertEquals(listOf(MetadataField.GENRES), admin.enrichments.single().lockedFields)
            assertFalse(viewModel.state.value.hasUnsavedLocks)
        }

    @Test
    fun `a refused save keeps the ticked locks and says what failed`() =
        runTest {
            val admin = FakeAdminRepository().apply { enrichments += AdminFixtures.enrichment(mediaId = "dark") }
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()

            viewModel.toggleLock(MetadataField.TITLE)
            admin.matchingFailWith = BeamException.Forbidden("admin only", "about:blank")
            viewModel.saveLocks()
            testScheduler.advanceUntilIdle()

            val state = viewModel.state.value
            assertEquals(setOf(MetadataField.TITLE), state.locks)
            assertTrue(state.hasUnsavedLocks)
            assertTrue(state.message!!.startsWith("The locks could not be saved."))
        }

    @Test
    fun `a title the server does not know is a failure, not an empty page`() =
        runTest {
            val viewModel = TitleMetadataViewModel(FakeAdminRepository())
            viewModel.open("gone")
            testScheduler.advanceUntilIdle()

            assertNull(viewModel.state.value.enrichment)
            assertNotNull(viewModel.state.value.error)
        }

    @Test
    fun `a title that failed to read is read again on retry, without leaving the page`() =
        runTest {
            val admin =
                FakeAdminRepository().apply {
                    enrichments += AdminFixtures.enrichment(mediaId = "dark")
                    failWith = BeamException.Network("connection reset", retryable = true)
                }
            val viewModel = TitleMetadataViewModel(admin)
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()
            assertNotNull(viewModel.state.value.error)

            // Opening the same title again is a configuration change, and reads nothing.
            admin.failWith = null
            viewModel.open("dark")
            testScheduler.advanceUntilIdle()
            assertNotNull(viewModel.state.value.error)

            viewModel.retry()
            testScheduler.advanceUntilIdle()
            assertNull(viewModel.state.value.error)
            assertEquals(
                "dark",
                viewModel.state.value.enrichment
                    ?.mediaId,
            )
        }

    @Test
    fun `a show offers no release date or runtime to lock`() {
        val show = lockableFields(MediaKind.SHOW)
        assertFalse(MetadataField.RELEASE_DATE in show)
        assertFalse(MetadataField.RUNTIME in show)
        assertEquals(MetadataField.entries, lockableFields(MediaKind.MOVIE))
    }

    @Test
    fun `a title's detail says why it is unmatched, what pins it and what is locked`() {
        val detail =
            enrichmentDetail(
                AdminFixtures.enrichment(
                    pinnedRef = "tmdb:1",
                    pinSource = PinSource.NFO,
                    lockedFields = listOf(MetadataField.ORIGINAL_TITLE),
                    lastError = "no candidate",
                ),
            )
        assertEquals("no candidate\nPinned to tmdb:1 by an NFO file\nLocked: original title", detail)
        assertNull(enrichmentDetail(AdminFixtures.enrichment(lastError = null)))
    }
}
