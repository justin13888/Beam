package dev.beam.android.core.ffi

import android.content.Context
import android.content.res.Configuration
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.view.Display
import android.view.WindowManager
import uniffi.beam_client_core.DecoderCapability
import uniffi.beam_client_core.DeviceProfile

/**
 * Asks the platform what this device can actually decode.
 *
 * This is the reason a native client exists at all. Beam never transcodes
 * (ADR-0004), so playback succeeds exactly when the device has a decoder for
 * what is on disk. A browser cannot answer that question for HEVC or AV1;
 * `MediaCodecList` can, and the answer it gives is what the core matches
 * sources against.
 */
public object DeviceProfiles {

    /**
     * Containers ExoPlayer can demux.
     *
     * Read from a fixed list rather than from the platform, because container
     * support is an ExoPlayer property and not a `MediaCodec` one -- there is
     * no API that reports it. Matroska is the entry that matters: it is what
     * most remuxed libraries use and what a browser most often refuses.
     */
    private val SupportedContainers = listOf(
        "mp4", "m4v", "mov", "mkv", "webm", "ts", "mpegts", "avi", "flv", "ogg", "3gp",
    )

    /**
     * Build a profile describing this device.
     *
     * @param allowSoftwareDecode whether files that only a software decoder
     *   can handle should count as playable.
     */
    public fun build(context: Context, allowSoftwareDecode: Boolean): DeviceProfile {
        val codecs = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
        val video = mutableListOf<DecoderCapability>()
        val audio = mutableListOf<DecoderCapability>()

        for (info in codecs) {
            if (info.isEncoder) continue
            val hardware = isHardwareAccelerated(info)
            for (mime in info.supportedTypes) {
                val capabilities = runCatching { info.getCapabilitiesForType(mime) }.getOrNull()
                    ?: continue
                when {
                    mime.startsWith("video/") ->
                        video += videoCapability(mime, capabilities, hardware)

                    mime.startsWith("audio/") ->
                        audio += audioCapability(mime, hardware)
                }
            }
        }

        val (width, height) = displaySize(context)
        return DeviceProfile(
            videoDecoders = video,
            audioDecoders = audio,
            supportedContainers = SupportedContainers,
            displayWidth = width.toUInt(),
            displayHeight = height.toUInt(),
            displaySupportsHdr = supportsHdr(context),
            preferredAudioLanguages = preferredLanguages(context),
            allowSoftwareDecode = allowSoftwareDecode,
        )
    }

    private fun videoCapability(
        mime: String,
        capabilities: MediaCodecInfo.CodecCapabilities,
        hardware: Boolean,
    ): DecoderCapability {
        val video = capabilities.videoCapabilities
        val profiles = capabilities.profileLevels.map { it.profile }.toSet()
        return DecoderCapability(
            mimeType = mime,
            isHardwareAccelerated = hardware,
            maxWidth = video?.supportedWidths?.upper?.toUInt(),
            maxHeight = video?.supportedHeights?.upper?.toUInt(),
            maxBitrateBps = video?.bitrateRange?.upper?.toULong(),
            supportsHdr10 = profiles.any { it in Hdr10Profiles },
            supportsDolbyVision = mime.equals(MediaFormat.MIMETYPE_VIDEO_DOLBY_VISION, true) ||
                profiles.any { it in DolbyVisionProfiles },
            // A decoder that only advertises 8-bit profiles cannot play a
            // 10-bit stream even at a resolution it otherwise accepts, which
            // is the failure that looks like a green or black picture rather
            // than an error.
            supports10Bit = profiles.any { it in TenBitProfiles },
        )
    }

    private fun audioCapability(mime: String, hardware: Boolean): DecoderCapability =
        DecoderCapability(
            mimeType = mime,
            isHardwareAccelerated = hardware,
            // Audio decoders have no meaningful size or bitrate ceiling to
            // report, and inventing one would reject files that play fine.
            maxWidth = null,
            maxHeight = null,
            maxBitrateBps = null,
            supportsHdr10 = false,
            supportsDolbyVision = false,
            supports10Bit = false,
        )

    /**
     * Whether a codec runs on dedicated silicon.
     *
     * `isHardwareAccelerated` only exists from API 29. Below that the standard
     * heuristic is the codec name: Google's software decoders are prefixed
     * `OMX.google.` or `c2.android.`, and everything else is a vendor codec.
     */
    private fun isHardwareAccelerated(info: MediaCodecInfo): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            info.isHardwareAccelerated
        } else {
            val name = info.name.lowercase()
            !name.startsWith("omx.google.") && !name.startsWith("c2.android.")
        }

    private fun displaySize(context: Context): Pair<Int, Int> {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val metrics = context.getSystemService(WindowManager::class.java)
                ?.maximumWindowMetrics
            val bounds = metrics?.bounds
            if (bounds != null) return bounds.width() to bounds.height()
        }
        val display = context.getSystemService(WindowManager::class.java)?.defaultDisplay
        @Suppress("DEPRECATION")
        return (display?.width ?: 1920) to (display?.height ?: 1080)
    }

    private fun supportsHdr(context: Context): Boolean {
        val display: Display = context.getSystemService(WindowManager::class.java)
            ?.defaultDisplay ?: return false
        @Suppress("DEPRECATION")
        val types = display.hdrCapabilities?.supportedHdrTypes ?: return false
        return types.isNotEmpty()
    }

    /**
     * The user's own language preferences, best first.
     *
     * Taken from the system locale list rather than an app setting, so a
     * bilingual user's second language is honoured without configuring
     * anything.
     */
    private fun preferredLanguages(context: Context): List<String> {
        val configuration: Configuration = context.resources.configuration
        val locales = configuration.locales
        return (0 until locales.size()).mapNotNull { index ->
            locales[index]?.isO3Language?.takeIf { it.isNotBlank() }
        }.distinct()
    }

    // The SDK groups profile constants by codec, not by capability, so the
    // sets a player actually cares about have to be assembled by hand. Each
    // one spans every codec that can carry the feature, because the question
    // being asked is "can this device show HDR10", not "which codec is it".

    /** Profiles that carry an HDR10 or HDR10+ signal. */
    private val Hdr10Profiles = setOf(
        MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10HDR10,
        MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10HDR10Plus,
        MediaCodecInfo.CodecProfileLevel.VP9Profile2HDR,
        MediaCodecInfo.CodecProfileLevel.VP9Profile3HDR,
        MediaCodecInfo.CodecProfileLevel.VP9Profile2HDR10Plus,
        MediaCodecInfo.CodecProfileLevel.VP9Profile3HDR10Plus,
        MediaCodecInfo.CodecProfileLevel.AV1ProfileMain10HDR10,
        MediaCodecInfo.CodecProfileLevel.AV1ProfileMain10HDR10Plus,
    )

    /** Dolby Vision profile constants, which have no named group in the SDK. */
    private val DolbyVisionProfiles = setOf(
        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheDtr,
        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvheSt,
        MediaCodecInfo.CodecProfileLevel.DolbyVisionProfileDvavSe,
    )

    /** Profiles with a bit depth above 8, HDR ones included. */
    private val TenBitProfiles = Hdr10Profiles + setOf(
        MediaCodecInfo.CodecProfileLevel.AVCProfileHigh10,
        MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10,
        MediaCodecInfo.CodecProfileLevel.VP9Profile2,
        MediaCodecInfo.CodecProfileLevel.VP9Profile3,
        MediaCodecInfo.CodecProfileLevel.AV1ProfileMain10,
    )
}
