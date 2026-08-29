package dev.beam.android.core.model

/**
 * The state of something being loaded, as a screen renders it.
 *
 * Modelled as a sealed hierarchy rather than a struct of nullable fields so a
 * screen cannot render "loading" and "error" at once, and so `when` is
 * exhaustive over the cases that actually exist.
 */
public sealed interface LoadState<out T> {

    /** Nothing has been requested yet. */
    public data object Idle : LoadState<Nothing>

    /**
     * A request is in flight.
     *
     * @property previous the last successful value, kept so a refresh can keep
     *   showing content instead of flashing a spinner over it.
     */
    public data class Loading<out T>(val previous: T? = null) : LoadState<T>

    /** The request succeeded. */
    public data class Success<out T>(val value: T) : LoadState<T>

    /**
     * The request failed.
     *
     * @property message text already phrased for a person.
     * @property retryable whether offering a retry would be honest.
     * @property previous the last successful value, so a failed refresh can
     *   keep showing stale content with an error alongside it.
     */
    public data class Failure<out T>(
        val message: String,
        val retryable: Boolean,
        val previous: T? = null,
    ) : LoadState<T>
}

/** The value held, if this state has one -- current or stale. */
public val <T> LoadState<T>.valueOrNull: T?
    get() = when (this) {
        is LoadState.Success -> value
        is LoadState.Loading -> previous
        is LoadState.Failure -> previous
        LoadState.Idle -> null
    }

/** Whether a progress indicator belongs on screen. */
public val LoadState<*>.isLoading: Boolean
    get() = this is LoadState.Loading
