package dev.beam.android.feature.player

import androidx.lifecycle.SavedStateHandle
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import dagger.hilt.android.lifecycle.HiltViewModel
import dev.beam.android.core.ffi.preferences.PreferencesRepository
import dev.beam.android.core.ffi.repository.CatalogRepository
import dev.beam.android.core.ffi.repository.PlaybackRepository
import dev.beam.android.core.media.player.BeamPlayer
import dev.beam.android.core.media.player.PlaybackUiState
import dev.beam.android.core.media.session.PlayerProvider
import dev.beam.android.core.model.PlaybackRequest
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.beam_client_core.EpisodeSummary
import uniffi.beam_client_core.MediaSourceView
import javax.inject.Inject

/** What the player screen shows, beyond the player's own state. */
public data class PlayerScreenState(
    /** The player's own state. */
    val playback: PlaybackUiState = PlaybackUiState(),
    /** Alternative files, for the quality switcher. */
    val sources: List<MediaSourceView> = emptyList(),
    /** The episode queued to play next, when there is one. */
    val upNext: EpisodeSummary? = null,
    /** Whether the up-next prompt is showing. */
    val isOfferingNext: Boolean = false,
    /** Whether the transport controls are visible. */
    val areControlsVisible: Boolean = true,
    /** Whether the viewer wants the next episode to start on its own. */
    val autoPlayNext: Boolean = true,
    /** The automatic switch to a smaller file that just happened, while it is on screen. */
    val downgrade: SourceDowngrade? = null,
    /**
     * Whether the viewer chose the file themselves, which turns the automatic
     * downgrade off: a viewer who picked the remux on purpose is not
     * overridden, however much it stalls.
     */
    val isSourcePinned: Boolean = false,
)

/**
 * A switch to a smaller file after sustained rebuffering.
 *
 * Always shown. Beam does not do adaptive bitrate (ADR-0014), and a silent
 * switch would be pretending to: the viewer is told what happened and can
 * take it back.
 */
public data class SourceDowngrade(
    /** What to tell the viewer, phrased by the core. */
    val message: String,
    /** The file playback left, so undoing the switch can return to it. */
    val fromFileId: String,
)

/**
 * The player screen.
 *
 * Owns nothing about decoding -- that is [BeamPlayer]'s job -- and exists to
 * decide what happens *around* playback: which file, what plays next, and when
 * to offer it.
 */
@HiltViewModel
public class PlayerViewModel
    @Inject
    constructor(
        private val player: BeamPlayer,
        private val playerProvider: PlayerProvider,
        private val playback: PlaybackRepository,
        private val catalog: CatalogRepository,
        private val preferences: PreferencesRepository,
        savedStateHandle: SavedStateHandle,
    ) : ViewModel() {
        private val fileId: String =
            requireNotNull(savedStateHandle["fileId"]) {
                "the player needs a fileId"
            }
        private val mediaId: String? = savedStateHandle["mediaId"]
        private val episodeId: String? = savedStateHandle["episodeId"]

        private val mutableState = MutableStateFlow(PlayerScreenState())
        public val state: StateFlow<PlayerScreenState> = mutableState.asStateFlow()

        /** The file a downgrade was last attempted from, so one verdict asks once. */
        private var downgradeAttemptedFrom: String? = null

        /**
         * The instance the video surface renders from.
         *
         * Exposed rather than hidden behind [BeamPlayer] because a `PlayerView`
         * needs the real `Player` to attach a surface to; there is no way to
         * render video through an interface that does not name one. It is the same
         * singleton the media session publishes, so the notification and the
         * screen can never disagree about what is playing.
         */
        public val exoPlayer: androidx.media3.common.Player by lazy { playerProvider.exoPlayer() }

        init {
            viewModelScope.launch {
                val autoPlay = preferences.preferences.first().autoPlayNext
                mutableState.update { it.copy(autoPlayNext = autoPlay) }
            }
            viewModelScope.launch {
                player.state.collect { playbackState ->
                    mutableState.update { it.copy(playback = playbackState) }
                    if (playbackState.hasEnded) onEnded()
                    if (playbackState.isRebufferingSustained) onSustainedRebuffering(playbackState)
                }
            }
            viewModelScope.launch {
                mediaId?.let { id ->
                    runCatching { playback.sources(id) }
                        .onSuccess { sources -> mutableState.update { it.copy(sources = sources) } }
                }
            }
            viewModelScope.launch {
                if (mediaId != null && episodeId != null) {
                    runCatching { catalog.upNext(mediaId, episodeId) }
                        .onSuccess { next -> mutableState.update { it.copy(upNext = next) } }
                }
            }
        }

        /**
         * Begin playback.
         *
         * A file the viewer picked before playback starts pinned, exactly as a
         * pick made in the player would. It only ever adds a pin: the route
         * calls this again when it is recomposed from scratch, such as after a
         * rotation, and that must not drop a pin set in the player since.
         */
        public fun start(request: PlaybackRequest) {
            mutableState.update { it.copy(isSourcePinned = it.isSourcePinned || request.isSourcePicked) }
            viewModelScope.launch { player.play(request) }
        }

        /** Toggle between playing and paused. */
        public fun togglePlayPause() {
            if (mutableState.value.playback.isPlaying) player.pause() else player.resume()
        }

        /** Seek to an absolute position. */
        public fun seekTo(positionMs: Long): Unit = player.seekTo(positionMs)

        /** Skip backwards by the standard interval. */
        public fun rewind(): Unit = player.seekBy(-SKIP_BACK_MS)

        /** Skip forwards by the standard interval. */
        public fun fastForward(): Unit = player.seekBy(SKIP_FORWARD_MS)

        /** Change playback speed. */
        public fun setSpeed(speed: Float): Unit = player.setSpeed(speed)

        /** Choose an audio track. */
        public fun selectAudioTrack(id: String): Unit = player.selectAudioTrack(id)

        /** Choose a subtitle track, or turn subtitles off. */
        public fun selectSubtitleTrack(id: String?): Unit = player.selectSubtitleTrack(id)

        /**
         * Switch to a different file, holding position.
         *
         * The viewer's own choice, so it pins the source: no automatic
         * downgrade overrides it afterwards.
         */
        public fun switchSource(source: MediaSourceView) {
            mutableState.update { it.copy(isSourcePinned = true, downgrade = null) }
            viewModelScope.launch { player.switchSource(source.fileId) }
        }

        /**
         * Take back an automatic downgrade: return to the file it left, and
         * pin it, since going back is the viewer saying they want that file.
         */
        public fun undoDowngrade() {
            val downgrade = mutableState.value.downgrade ?: return
            mutableState.update { it.copy(isSourcePinned = true, downgrade = null) }
            viewModelScope.launch { player.switchSource(downgrade.fromFileId) }
        }

        /** Hide the downgrade notice, keeping the smaller file. */
        public fun dismissDowngrade() {
            mutableState.update { it.copy(downgrade = null) }
        }

        /** Show or hide the transport controls. */
        public fun setControlsVisible(visible: Boolean) {
            mutableState.update { it.copy(areControlsVisible = visible) }
        }

        /** Dismiss the up-next prompt without playing it. */
        public fun dismissUpNext() {
            mutableState.update { it.copy(isOfferingNext = false) }
        }

        /** Play the queued episode now. */
        public fun playNext() {
            val next = mutableState.value.upNext ?: return
            val nextFile = next.fileId ?: return
            // A different title: a pin on this episode's file says nothing about
            // which file of the next one the viewer wants.
            mutableState.update { it.copy(isOfferingNext = false, isSourcePinned = false, downgrade = null) }
            viewModelScope.launch {
                player.play(
                    PlaybackRequest(
                        mediaId = mediaId.orEmpty(),
                        episodeId = next.id,
                        fileId = nextFile,
                        title = next.title,
                        subtitle = null,
                        artworkUrl = next.thumbnailUrl,
                        durationSecs = next.durationSecs,
                    ),
                )
            }
        }

        /** Stop and release the decoder. */
        public fun stop(): Unit = player.stop()

        private fun onEnded() {
            val next = mutableState.value.upNext
            // Auto-advance only when there is somewhere to advance *to*. Offering
            // a prompt with nothing behind it, or auto-playing into an episode
            // whose file this server does not have, is worse than simply stopping.
            if (next?.fileId == null) return
            if (mutableState.value.autoPlayNext) {
                playNext()
            } else {
                mutableState.update { it.copy(isOfferingNext = true) }
            }
        }

        private fun onSustainedRebuffering(playbackState: PlaybackUiState) {
            val request = playbackState.request ?: return
            if (mutableState.value.isSourcePinned) return
            if (downgradeAttemptedFrom == request.fileId) return
            downgradeAttemptedFrom = request.fileId
            // Sources belong to a movie or an episode; a show has none of its
            // own, so an episode is asked about by its own id.
            val titleId = request.episodeId ?: request.mediaId.takeIf { it.isNotEmpty() } ?: return
            viewModelScope.launch {
                val selection =
                    runCatching { playback.downgradeSource(titleId, request.fileId) }.getOrNull()
                        ?: return@launch
                // The viewer may have picked a file, or moved on, while the
                // core was asked: either way this answer is about a file that
                // is no longer the question.
                if (mutableState.value.isSourcePinned) return@launch
                if (player.state.value.request
                        ?.fileId != request.fileId
                ) {
                    return@launch
                }
                mutableState.update {
                    it.copy(downgrade = SourceDowngrade(message = selection.reason, fromFileId = request.fileId))
                }
                player.switchSource(selection.source.fileId)
            }
        }

        override fun onCleared() {
            // Deliberately not `release()`: the player is a singleton shared with
            // the media session, so releasing it here would kill background
            // playback the moment the screen went away.
            player.stop()
        }

        private companion object {
            const val SKIP_BACK_MS = 10_000L
            const val SKIP_FORWARD_MS = 30_000L
        }
    }
