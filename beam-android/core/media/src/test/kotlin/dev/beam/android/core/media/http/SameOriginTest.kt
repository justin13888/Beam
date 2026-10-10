package dev.beam.android.core.media.http

import okhttp3.HttpUrl.Companion.toHttpUrl
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The predicate that decides where the session cookie may go. Each case differs
 * from the signed-in server's origin in one respect, so scheme, host and port
 * are each held to account on their own.
 */
class SameOriginTest {
    private data class Case(
        val name: String,
        val origin: String,
        val url: String,
        val same: Boolean,
    )

    private val cases =
        listOf(
            Case("the same origin", "https://beam.example:8443", "https://beam.example:8443/v1/artwork/x", same = true),
            Case(
                "another port on the same host",
                "https://beam.example:8443",
                "https://beam.example:9443/x",
                same = false,
            ),
            Case(
                "plain http on the same host and port",
                "https://beam.example:8443",
                "http://beam.example:8443/x",
                same = false,
            ),
            Case(
                "another host on the same port",
                "https://beam.example:8443",
                "https://other.example:8443/x",
                same = false,
            ),
            Case("the host in another case", "https://beam.example:8443", "https://BEAM.Example:8443/x", same = true),
            Case("the default port written out", "https://beam.example", "https://beam.example:443/x", same = true),
            Case("the default port left implicit", "http://beam.example:80", "http://beam.example/x", same = true),
        )

    @Test
    fun `an origin is scheme, host and port together`() {
        for ((name, origin, url, same) in cases) {
            assertEquals(name, same, url.toHttpUrl().sameOrigin(origin.toHttpUrl()))
        }
    }
}
