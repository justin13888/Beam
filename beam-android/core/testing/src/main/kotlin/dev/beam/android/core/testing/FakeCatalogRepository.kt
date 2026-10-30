package dev.beam.android.core.testing

import dev.beam.android.core.ffi.repository.CatalogRepository
import uniffi.beam_client_core.BeamException
import uniffi.beam_client_core.BrowseQuery
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.LibraryFileSummary
import uniffi.beam_client_core.LibrarySummary
import uniffi.beam_client_core.MediaDetail
import uniffi.beam_client_core.MediaPage

/** A catalog that answers from memory. */
public class FakeCatalogRepository : CatalogRepository {
    /** Set to make every call throw, for testing failure rendering. */
    public var failWith: BeamException? = null

    /** Pages returned by [browse], in call order; the last repeats. */
    public var pages: List<MediaPage> = listOf(Fixtures.page())

    /** Details keyed by media id. */
    public var details: MutableMap<String, MediaDetail> = mutableMapOf()

    /** Genres returned by [genres]. */
    public var genreList: List<String> = listOf("Crime", "Drama", "Thriller")

    /** Libraries returned by [libraries]. */
    public var libraryList: List<LibrarySummary> = listOf(Fixtures.library())

    /** Files returned by [libraryFiles]. */
    public var files: List<LibraryFileSummary> = emptyList()

    /**
     * Set to make [libraryFiles] alone fail.
     *
     * A library whose files cannot be read is not the same as a library that
     * is gone, and the screen treats them differently; one failure mode for
     * both could not express that.
     */
    public var filesFailWith: BeamException? = null

    /** The episode [upNext] resolves to. */
    public var nextEpisode: EpisodeSummary? = null

    /** Every query [browse] was called with, in order. */
    public val browseCalls: MutableList<BrowseQuery> = mutableListOf()

    /**
     * The cursor every page reports, or null for "this is the last page".
     *
     * Overrides whatever [pages] carries, so a paging test can be written
     * without constructing a page fixture per page.
     */
    public var nextCursor: String? = null

    /**
     * Set to make [genres] alone fail.
     *
     * Distinct from [failWith] because the genre chips and the catalog grid
     * are separate concerns: one failing must not be indistinguishable from
     * both failing.
     */
    public var genresFailWith: BeamException? = null

    override suspend fun browse(query: BrowseQuery): MediaPage {
        failWith?.let { throw it }
        browseCalls += query
        val page = pages.getOrElse(browseCalls.size - 1) { pages.last() }
        return page.copy(endCursor = nextCursor, hasNextPage = nextCursor != null)
    }

    override suspend fun detail(mediaId: String): MediaDetail {
        failWith?.let { throw it }
        return details[mediaId]
            ?: throw BeamException.NotFound("No title with id $mediaId", "about:blank")
    }

    override suspend fun genres(): List<String> {
        genresFailWith?.let { throw it }
        failWith?.let { throw it }
        return genreList
    }

    override suspend fun libraries(): List<LibrarySummary> {
        failWith?.let { throw it }
        return libraryList
    }

    override suspend fun library(libraryId: String): LibrarySummary {
        failWith?.let { throw it }
        return libraryList.firstOrNull { it.id == libraryId }
            ?: throw BeamException.NotFound("No library with id $libraryId", "about:blank")
    }

    override suspend fun libraryFiles(libraryId: String): List<LibraryFileSummary> {
        filesFailWith?.let { throw it }
        failWith?.let { throw it }
        return files
    }

    /**
     * The episode after each episode, keyed by the current one: a series the
     * player can advance through more than once. [nextEpisode] answers for
     * any episode not listed here.
     */
    public val nextEpisodes: MutableMap<String, EpisodeSummary> = mutableMapOf()

    /** Every `(showId, currentEpisodeId)` [upNext] was asked about, in order. */
    public val upNextCalls: MutableList<Pair<String, String>> = mutableListOf()

    override suspend fun upNext(
        showId: String,
        currentEpisodeId: String,
    ): EpisodeSummary? {
        upNextCalls += showId to currentEpisodeId
        failWith?.let { throw it }
        return nextEpisodes[currentEpisodeId] ?: nextEpisode
    }
}
