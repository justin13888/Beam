package dev.beam.android.core.media.download

import dev.beam.android.core.model.DownloadRecord
import dev.beam.android.core.model.ThemeMode
import dev.beam.android.core.model.UserPreferences
import dev.beam.android.core.testing.FakePreferencesRepository
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The download repository follows the stored cellular preference itself, so
 * a process the system starts for the download service alone honours it too.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class CellularPreferenceTest {
    @Test
    fun `the stored cellular preference reaches downloads, and each change of it`() =
        runTest {
            val downloads = RecordingDownloadRepository()
            val preferences = FakePreferencesRepository(UserPreferences(downloadOverCellular = true))
            val following = launch { followCellularPreference(preferences, downloads) }
            testScheduler.advanceUntilIdle()
            assertEquals(
                "the stored choice applies with no screen involved",
                listOf(false),
                downloads.unmeteredRequirements,
            )

            preferences.update { it.copy(downloadOverCellular = false) }
            testScheduler.advanceUntilIdle()
            preferences.update { it.copy(themeMode = ThemeMode.Dark) }
            testScheduler.advanceUntilIdle()
            following.cancel()

            assertEquals(
                "forbidding cellular restores the requirement, and an unrelated change resends nothing",
                listOf(false, true),
                downloads.unmeteredRequirements,
            )
        }

    /** Records the network requirement it is given; nothing here downloads. */
    private class RecordingDownloadRepository : DownloadRepository {
        /** Every value passed to [setRequiresUnmeteredNetwork], in order. */
        val unmeteredRequirements: MutableList<Boolean> = mutableListOf()

        override val downloads: Flow<List<DownloadRecord>> = flowOf(emptyList())

        override suspend fun enqueue(
            fileId: String,
            serverId: String,
            mediaId: String,
            title: String,
            subtitle: String?,
            posterUrl: String?,
        ) = Unit

        override suspend fun pause(fileId: String) = Unit

        override suspend fun resume(fileId: String) = Unit

        override suspend fun remove(fileId: String) = Unit

        override suspend fun removeAll() = Unit

        override suspend fun setRequiresUnmeteredNetwork(required: Boolean) {
            unmeteredRequirements += required
        }

        override suspend fun isDownloaded(fileId: String): Boolean = false
    }
}
