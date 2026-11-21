package dev.beam.android.core.ffi.repository

import uniffi.beam_client_core.BeamClient
import uniffi.beam_client_core.ContinueWatchingEntry
import uniffi.beam_client_core.DeviceProfile
import uniffi.beam_client_core.HistoryPage
import uniffi.beam_client_core.MediaSourceView
import uniffi.beam_client_core.PlaybackHttpConfig
import uniffi.beam_client_core.PlaybackPlan
import uniffi.beam_client_core.ProgressDrain
import uniffi.beam_client_core.ProgressOutcome
import uniffi.beam_client_core.QualityPolicy
import uniffi.beam_client_core.SourceSelection
import javax.inject.Inject
import javax.inject.Singleton

@Singleton
internal class BeamPlaybackRepository
    @Inject
    constructor(
        private val client: BeamClient,
    ) : PlaybackRepository {
        override suspend fun setDeviceProfile(profile: DeviceProfile) {
            client.setDeviceProfile(profile)
        }

        override suspend fun sources(mediaId: String): List<MediaSourceView> = client.mediaSources(mediaId)

        override suspend fun selectSource(
            mediaId: String,
            policy: QualityPolicy,
        ): SourceSelection = client.selectPlaybackSource(mediaId, policy)

        override suspend fun downgradeSource(
            titleId: String,
            currentFileId: String,
        ): SourceSelection? = client.selectDowngradeSource(titleId, currentFileId)

        override suspend fun playbackConfig(fileId: String): PlaybackHttpConfig = client.playbackConfig(fileId)

        override suspend fun playbackPlan(
            titleId: String?,
            fileId: String,
        ): PlaybackPlan = client.playbackPlan(titleId, fileId)

        override suspend fun markWatched(id: String) = client.markWatched(id)

        override suspend fun markUnwatched(id: String) = client.markUnwatched(id)

        override suspend fun continueWatching(limit: UInt?): List<ContinueWatchingEntry> =
            client.continueWatching(limit)

        override suspend fun history(
            first: UInt?,
            after: String?,
        ): HistoryPage = client.history(first, after)

        // A sample refused for credentials is queued rather than thrown; a
        // `401` behind it has already been announced by the core.
        override suspend fun reportProgress(
            fileId: String,
            positionSecs: Double,
            durationSecs: Double?,
            force: Boolean,
        ): ProgressOutcome = client.reportProgress(fileId, positionSecs, durationSecs, force)

        // A refused session is named, not thrown; the core announced the `401`.
        override suspend fun drainProgress(): ProgressDrain = client.drainProgress()

        override fun progressRetryDelaySecs(attempt: UInt): ULong =
            uniffi.beam_client_core.progressRetryDelaySecs(attempt)

        override suspend fun localStartPositionSecs(fileId: String): Double = client.localStartPositionSecs(fileId)
    }
