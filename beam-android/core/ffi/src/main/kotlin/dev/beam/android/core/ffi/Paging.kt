package dev.beam.android.core.ffi

import uniffi.beam_client_core.PageAdvance

/**
 * The rows of this page that [advance] says are new, in payload order, each
 * once.
 *
 * The core's `CursorPager` decides which identifiers it has already seen; this
 * only picks the rows those identifiers belong to, so a row the shifting list
 * repeated on a later page is not listed twice.
 */
public fun <T> List<T>.newIn(
    advance: PageAdvance,
    id: (T) -> String,
): List<T> {
    val remaining = advance.newIds.toMutableSet()
    return filter { row -> remaining.remove(id(row)) }
}
