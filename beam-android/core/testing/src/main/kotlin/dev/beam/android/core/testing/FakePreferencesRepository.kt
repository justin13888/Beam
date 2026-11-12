package dev.beam.android.core.testing

import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.model.UserPreferences
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow

/**
 * Preferences held in memory.
 *
 * A real [dev.beam.android.core.ffi.preferences.PreferencesRepository] is
 * DataStore-backed, which means a file, a coroutine scope and a real
 * filesystem -- none of which a view-model test should need.
 */
public class FakePreferencesRepository(
    initial: UserPreferences = UserPreferences(),
) : PreferencesRepository {
    private val state = MutableStateFlow(initial)

    override val preferences: Flow<UserPreferences> = state

    /** The current value, for assertions. */
    public val current: UserPreferences get() = state.value

    /** How many times [update] was called. */
    public var updateCount: Int = 0
        private set

    override suspend fun update(transform: (UserPreferences) -> UserPreferences) {
        updateCount++
        state.value = transform(state.value)
    }
}
