package dev.beam.android.core.ffi

import android.annotation.SuppressLint
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
    private val SupportedContainers =
        listOf(
            "mp4",
            "m4v",
            "mov",
            "mkv",
            "webm",
            "ts",
            "mpegts",
            "avi",
            "flv",
            "ogg",
            "3gp",
        )

    /**
     * Build a profile describing this device.
     *
     * @param allowSoftwareDecode whether files that only a software decoder
     *   can handle should count as playable.
     */
    public fun build(
        context: Context,
        allowSoftwareDecode: Boolean,
    ): DeviceProfile {
        val codecs = MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos
        val video = mutableListOf<DecoderCapability>()
        val audio = mutableListOf<DecoderCapability>()

        for (info in codecs) {
            if (info.isEncoder) continue
            val hardware = isHardwareAccelerated(info)
            for (mime in info.supportedTypes) {
                val capabilities =
                    runCatching { info.getCapabilitiesForType(mime) }.getOrNull()
                        ?: continue
                when {
                    mime.startsWith("video/") -> {
                        video += videoCapability(mime, capabilities, hardware)
                    }

                    mime.startsWith("audio/") -> {
                        audio += audioCapability(mime, hardware)
                    }
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
        // Profile numbers only mean something under the MIME type that
        // reported them, so each set is looked up under the decoder's own.
        val key = mime.lowercase()
        return DecoderCapability(
            mimeType = mime,
            isHardwareAccelerated = hardware,
            maxWidth = video?.supportedWidths?.upper?.toUInt(),
            maxHeight = video?.supportedHeights?.upper?.toUInt(),
            maxBitrateBps = video?.bitrateRange?.upper?.toULong(),
            supportsHdr10 = profiles.any { it in Hdr10Profiles[key].orEmpty() },
            // Dolby Vision profile constants are only ever reported under the
            // Dolby Vision MIME type, so the MIME type alone decides this.
            supportsDolbyVision = key == MediaFormat.MIMETYPE_VIDEO_DOLBY_VISION,
            // A decoder that only advertises 8-bit profiles cannot play a
            // 10-bit stream even at a resolution it otherwise accepts, which
            // is the failure that looks like a green or black picture rather
            // than an error.
            supports10Bit = profiles.any { it in TenBitProfiles[key].orEmpty() },
        )
    }

    private fun audioCapability(
        mime: String,
        hardware: Boolean,
    ): DecoderCapability =
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
            val metrics =
                context
                    .getSystemService(WindowManager::class.java)
                    ?.maximumWindowMetrics
            val bounds = metrics?.bounds
            if (bounds != null) return bounds.width() to bounds.height()
        }
        val display = context.getSystemService(WindowManager::class.java)?.defaultDisplay
        @Suppress("DEPRECATION")
        return (display?.width ?: 1920) to (display?.height ?: 1080)
    }

    /**
     * Whether the display renders any HDR format.
     *
     * From API 34 the HDR types are a property of the display mode, and
     * `Display.hdrCapabilities` is deprecated in its favour; below that the
     * capabilities object is the only source.
     */
    private fun supportsHdr(context: Context): Boolean {
        val display: Display =
            context
                .getSystemService(WindowManager::class.java)
                ?.defaultDisplay ?: return false

        val types =
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                display.mode.supportedHdrTypes
            } else {
                @Suppress("DEPRECATION")
                display.hdrCapabilities?.supportedHdrTypes ?: return false
            }
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
        return (0 until locales.size())
            .mapNotNull { index ->
                locales[index]?.isO3Language?.takeIf { it.isNotBlank() }
            }.distinct()
    }

    // Android numbers profiles per codec, not globally: AVCProfileMain and
    // HEVCProfileMain10 are both 0x02, and AVCProfileHigh10 is the same 0x10
    // as DolbyVisionProfileDvheDtr. So every set is keyed by the MIME type
    // whose constants it holds, and a decoder's profiles are only ever looked
    // up under its own MIME type -- a flat set would make every H.264 Main or
    // High decoder read as 10-bit (#395).
    //
    // InlinedApi is suppressed deliberately, and it is safe here in a way it
    // is not generally. These are `static final int` constants, so javac bakes
    // the literal into the bytecode -- there is no field lookup at runtime and
    // therefore no NoSuchFieldError on an older platform. They are only ever
    // compared against the profile numbers the device's own `MediaCodecInfo`
    // reports, so on a device that predates a constant, nothing reports that
    // value and the comparison simply never matches. That is the correct
    // answer: such a device genuinely cannot decode that profile.

    /** Profiles that carry an HDR10 or HDR10+ signal, by MIME type. */
    @SuppressLint("InlinedApi")
    private val Hdr10Profiles: Map<String, Set<Int>> =
        mapOf(
            MediaFormat.MIMETYPE_VIDEO_HEVC to
                setOf(
                    MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10HDR10,
                    MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10HDR10Plus,
                ),
            MediaFormat.MIMETYPE_VIDEO_VP9 to
                setOf(
                    MediaCodecInfo.CodecProfileLevel.VP9Profile2HDR,
                    MediaCodecInfo.CodecProfileLevel.VP9Profile3HDR,
                    MediaCodecInfo.CodecProfileLevel.VP9Profile2HDR10Plus,
                    MediaCodecInfo.CodecProfileLevel.VP9Profile3HDR10Plus,
                ),
            MediaFormat.MIMETYPE_VIDEO_AV1 to
                setOf(
                    MediaCodecInfo.CodecProfileLevel.AV1ProfileMain10HDR10,
                    MediaCodecInfo.CodecProfileLevel.AV1ProfileMain10HDR10Plus,
                ),
        )

    /**
     * Profiles with a bit depth above 8, by MIME type, HDR ones included.
     *
     * AVC High 4:2:2 and High 4:4:4 Predictive both allow more than 8 bits
     * per sample, so a decoder advertising either can take a 10-bit stream.
     */
    @SuppressLint("InlinedApi")
    private val TenBitProfiles: Map<String, Set<Int>> =
        mapOf(
            MediaFormat.MIMETYPE_VIDEO_AVC to
                setOf(
                    MediaCodecInfo.CodecProfileLevel.AVCProfileHigh10,
                    MediaCodecInfo.CodecProfileLevel.AVCProfileHigh422,
                    MediaCodecInfo.CodecProfileLevel.AVCProfileHigh444,
                ),
            MediaFormat.MIMETYPE_VIDEO_HEVC to
                setOf(MediaCodecInfo.CodecProfileLevel.HEVCProfileMain10),
            MediaFormat.MIMETYPE_VIDEO_VP9 to
                setOf(
                    MediaCodecInfo.CodecProfileLevel.VP9Profile2,
                    MediaCodecInfo.CodecProfileLevel.VP9Profile3,
                ),
            MediaFormat.MIMETYPE_VIDEO_AV1 to
                setOf(MediaCodecInfo.CodecProfileLevel.AV1ProfileMain10),
        ).let { tenBit ->
            (tenBit.keys + Hdr10Profiles.keys).associateWith { mime ->
                tenBit[mime].orEmpty() + Hdr10Profiles[mime].orEmpty()
            }
        }
}
