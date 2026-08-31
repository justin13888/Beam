package dev.beam.android.feature.detail

import dev.beam.android.core.model.QualityPreference
import uniffi.beam_client_core.QualityPolicy

/**
 * The viewer's stated preference, as the policy the core selects with.
 *
 * The mapping lives here rather than in the core because it is a *product*
 * decision about what the settings screen's words mean, and the core's
 * policies are the mechanism. Keeping them separate lets the wording change
 * without touching selection logic shared with every other platform.
 */
public fun QualityPreference.asPolicy(): QualityPolicy =
    when (this) {
        QualityPreference.Best -> QualityPolicy.Highest
        QualityPreference.MatchScreen -> QualityPolicy.MatchScreen
        QualityPreference.Smallest -> QualityPolicy.Smallest
    }
