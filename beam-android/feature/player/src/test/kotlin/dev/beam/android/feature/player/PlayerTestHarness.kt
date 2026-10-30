package dev.beam.android.feature.player

import androidx.lifecycle.SavedStateHandle
import dev.beam.android.core.media.session.PlayerProvider
import dev.beam.android.core.model.PlaybackRequest
import dev.beam.android.core.testing.FakeCatalogRepository
import dev.beam.android.core.testing.FakePlaybackRepository
import dev.beam.android.core.testing.FakePreferencesRepository
import dev.beam.android.core.testing.Fixtures
import uniffi.beam_client_core.Playability
import uniffi.beam_client_core.SourceSelection

// What PlayerTest, PlayerUpNextTest and PlayerSourceSwitchTest share: the
// view model under test, over fakes, and the requests and answers they feed it.

/** A [PlayerViewModel] over fakes, opened on episode-1's file-1 of media-1. */
internal fun viewModel(
    player: FakeBeamPlayer = FakeBeamPlayer(),
    catalog: FakeCatalogRepository =
        FakeCatalogRepository().apply {
            nextEpisode = Fixtures.episode(id = "episode-2", fileId = "file-2")
        },
    preferences: FakePreferencesRepository = FakePreferencesRepository(),
    playback: FakePlaybackRepository = FakePlaybackRepository(),
) = PlayerViewModel(
    player = player,
    // Throws rather than returning a stub: the view model must not build a
    // decoder simply to exist, and only the video surface should ever ask
    // for one. If this fires, that has regressed.
    playerProvider =
        object : PlayerProvider {
            override fun exoPlayer(): androidx.media3.exoplayer.ExoPlayer =
                error("the player screen must not construct a decoder to hold state")
        },
    playback = playback,
    catalog = catalog,
    preferences = preferences,
    savedStateHandle =
        SavedStateHandle(
            mapOf(
                "fileId" to "file-1",
                "mediaId" to "media-1",
                "episodeId" to "episode-1",
            ),
        ),
)

/** The core's answer to sustained rebuffering: a 480p file to fall back to. */
internal fun smallerSource() =
    SourceSelection(
        source = Fixtures.source(fileId = "file-sd", width = 854u, height = 480u),
        playability = Playability.Hardware,
        audioTrackIndex = null,
        reason = DOWNGRADE_REASON,
        rejected = emptyList(),
    )

/** The request that opened the screen [viewModel] builds. */
internal fun request() =
    PlaybackRequest(
        mediaId = "media-1",
        episodeId = "episode-1",
        fileId = "file-1",
        title = "Arrival",
    )

internal const val DOWNGRADE_REASON = "Switched to 480p because playback kept stalling"
