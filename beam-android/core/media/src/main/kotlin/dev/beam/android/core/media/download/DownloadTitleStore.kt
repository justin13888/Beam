package dev.beam.android.core.media.download

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import java.io.File

/** The display fields a download needs in order to render offline. */
@Serializable
public data class DownloadTitle(
    val fileId: String,
    val serverId: String,
    val mediaId: String,
    val episodeId: String? = null,
    val title: String,
    val subtitle: String? = null,
    val posterUrl: String? = null,
)

/**
 * Remembers what a downloaded file actually *is*.
 *
 * Media3's download index stores a URI and an opaque id and nothing else, so
 * without this the downloads screen could show a list of file identifiers and
 * a progress bar. Resolving those over the network would defeat the point of
 * an offline feature, so the titles are written down at enqueue time.
 */
public interface DownloadTitleStore {
    /** What this file is, if it is known. */
    public fun get(fileId: String): DownloadTitle?

    /** Record what a file is. */
    public fun put(title: DownloadTitle)

    /** Forget a file. */
    public fun remove(fileId: String)

    /** Everything known. */
    public fun all(): List<DownloadTitle>
}

/**
 * A [DownloadTitleStore] backed by one JSON file.
 *
 * A file rather than a database: the whole set is small, is read in full on
 * every render, and is written only when a download is added or removed. A
 * Room table would add a schema, a migration path and a compiler to the build
 * for a map of a few dozen entries.
 */
internal class FileDownloadTitleStore(
    context: Context,
    private val json: Json = Json { ignoreUnknownKeys = true },
) : DownloadTitleStore {
    private val file = File(context.filesDir, "download-titles.json")
    private val titles: MutableMap<String, DownloadTitle> = load().toMutableMap()

    override fun get(fileId: String): DownloadTitle? = synchronized(this) { titles[fileId] }

    override fun put(title: DownloadTitle): Unit =
        synchronized(this) {
            titles[title.fileId] = title
            persist()
        }

    override fun remove(fileId: String): Unit =
        synchronized(this) {
            titles.remove(fileId)
            persist()
        }

    override fun all(): List<DownloadTitle> = synchronized(this) { titles.values.toList() }

    private fun load(): Map<String, DownloadTitle> =
        runCatching {
            if (!file.exists()) return emptyMap()
            json
                .decodeFromString<List<DownloadTitle>>(file.readText())
                .associateBy(DownloadTitle::fileId)
        }.getOrElse {
            // A corrupt index costs the *labels* on existing downloads, not the
            // downloads themselves -- so it is recovered from rather than thrown,
            // which would make the screen unopenable.
            emptyMap()
        }

    private fun persist() {
        runCatching { file.writeText(json.encodeToString(titles.values.toList())) }
    }
}
