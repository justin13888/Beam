package dev.beam.android.core.media.download

import android.content.Context
import android.net.Uri
import coil3.ImageLoader
import coil3.request.CachePolicy
import coil3.request.ImageRequest
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import okio.Path.Companion.toOkioPath
import java.io.File
import java.io.IOException
import java.security.MessageDigest

/**
 * Keeps a download's poster on disk for exactly as long as the download exists.
 *
 * The downloads screen is the one screen guaranteed to be used offline, and a
 * poster URL written down at enqueue time is useless there unless its bytes
 * were fetched while the network was still up. So enqueueing fetches them and
 * keeps a copy in [directory], the downloads screen renders that copy (see
 * [displayUrl]), and removing the download deletes it.
 *
 * The fetch goes through the app's own [ImageLoader] rather than a second
 * fetcher. That loader fetches through
 * [dev.beam.android.core.media.http.ServerCallFactory], which attaches the
 * active server's session cookie and trust decision, so the poster is fetched
 * under the same credential as the API -- `/v1/artwork` answers 401 without
 * it. The bytes the loader writes to its disk cache are then copied out into
 * [directory].
 *
 * The copy is what makes the lifetime match. The loader's disk cache is a
 * shared LRU with a size cap, so enough browsing pushes any entry in it out;
 * a file of the app's own in [directory] is deleted only by [unpin], which the
 * download manager calls when the last download showing the poster goes.
 *
 * @param directory where pinned posters are kept, one file per poster URL.
 *   Under `filesDir` rather than `cacheDir`, because the system clears the
 *   cache directory under storage pressure and a pinned poster must outlive
 *   that just as the download's own bytes do.
 * @param loader resolved on first use rather than at construction, because the
 *   app's singleton loader is built from a client that is injected into the
 *   application after the graph that builds this class.
 */
public class DownloadArtwork(
    private val context: Context,
    private val directory: File = File(context.filesDir, DIRECTORY),
    private val loader: () -> ImageLoader,
) {
    /**
     * Fetch [url] and keep a copy of it until [unpin].
     *
     * Never throws for a network or storage failure: a poster is decoration on
     * a download the viewer asked for, and failing the download because the
     * poster would not load would be the wrong way round. A poster that could
     * not be kept is fetched again on the next enqueue of the same file.
     */
    public suspend fun pin(url: String) {
        val target = fileFor(url)
        if (withContext(Dispatchers.IO) { target.exists() }) return
        val images = loader()
        val request =
            ImageRequest
                .Builder(context)
                .data(url)
                .diskCacheKey(url)
                // The point is the bytes on disk. Filling the memory cache
                // would evict artwork the viewer is actually looking at in
                // exchange for a poster nobody is looking at yet.
                .memoryCachePolicy(CachePolicy.DISABLED)
                .diskCachePolicy(CachePolicy.ENABLED)
                // Coil caches the undecoded response, so the decode that
                // follows the fetch is pure cost here. A tiny target makes it
                // a subsampled decode of a few pixels instead of a full poster.
                .size(PIN_DECODE_SIZE_PX, PIN_DECODE_SIZE_PX)
                .build()
        // The result is deliberately not inspected: a failed fetch leaves no
        // disk cache entry, and the copy below then has nothing to copy.
        images.execute(request)
        withContext(Dispatchers.IO) { copyOut(images, url, target) }
    }

    /** Stop keeping [url]; it is fetched again on the next online render. */
    public fun unpin(url: String) {
        fileFor(url).delete()
        loader().diskCache?.remove(url)
    }

    /**
     * What the downloads screen should render for [url]: the kept copy where
     * there is one, so it renders with no network, and [url] itself otherwise,
     * so a poster that could not be kept still renders online.
     */
    public fun displayUrl(url: String): String {
        val kept = fileFor(url)
        return if (kept.isFile) Uri.fromFile(kept).toString() else url
    }

    private fun copyOut(
        images: ImageLoader,
        url: String,
        target: File,
    ) {
        val cache = images.diskCache ?: return
        val snapshot = cache.openSnapshot(url) ?: return
        // Written beside the target and renamed into place, so a reader never
        // sees half a poster and a crash mid-copy leaves nothing that looks
        // kept.
        val partial = File(directory, "${target.name}$PARTIAL_SUFFIX")
        snapshot.use {
            try {
                directory.mkdirs()
                cache.fileSystem.copy(it.data, partial.toOkioPath())
                if (!partial.renameTo(target)) partial.delete()
            } catch (_: IOException) {
                partial.delete()
            }
        }
    }

    /**
     * One file per URL, named by its SHA-256: an artwork URL holds `/` and
     * `:`, and a hash is the one name that is both a valid file name and
     * unique to the URL.
     */
    private fun fileFor(url: String): File {
        val digest = MessageDigest.getInstance("SHA-256").digest(url.toByteArray(Charsets.UTF_8))
        return File(directory, digest.joinToString("") { "%02x".format(it) })
    }

    private companion object {
        const val PIN_DECODE_SIZE_PX = 1
        const val DIRECTORY = "download-artwork"
        const val PARTIAL_SUFFIX = ".partial"
    }
}
