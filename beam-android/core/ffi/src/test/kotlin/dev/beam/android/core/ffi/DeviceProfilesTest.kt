package dev.beam.android.core.ffi

import android.media.MediaCodecInfo
import android.media.MediaCodecInfo.CodecProfileLevel
import android.media.MediaFormat
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.shadows.MediaCodecInfoBuilder
import org.robolectric.shadows.ShadowMediaCodecList

/**
 * What the device says it can decode, read from a codec list Robolectric
 * holds -- the platform API the profile is built from, with codecs this test
 * chooses rather than whichever ones the host happens to have.
 */
@RunWith(RobolectricTestRunner::class)
class DeviceProfilesTest {
    @Test
    fun `a main10 hevc decoder reports 10 bit`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.hevc.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_HEVC,
                profiles = intArrayOf(CodecProfileLevel.HEVCProfileMain, CodecProfileLevel.HEVCProfileMain10),
                hardware = true,
            ),
        )
        // Main and High share their numbers with HEVC Main10 and VP9 Profile
        // 3, so this AVC decoder only stays 8-bit if the lookup is keyed by
        // its own MIME type (#395).
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.android.avc.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_AVC,
                profiles = intArrayOf(CodecProfileLevel.AVCProfileMain, CodecProfileLevel.AVCProfileHigh),
                hardware = false,
            ),
        )

        val profile = DeviceProfiles.build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = false)

        val byMime = profile.videoDecoders.associateBy { it.mimeType }
        assertEquals(setOf(MediaFormat.MIMETYPE_VIDEO_HEVC, MediaFormat.MIMETYPE_VIDEO_AVC), byMime.keys)
        val hevc = byMime.getValue(MediaFormat.MIMETYPE_VIDEO_HEVC)
        assertTrue("Main10 is a 10-bit profile", hevc.supports10Bit)
        assertFalse("plain Main10 carries no HDR10 signal", hevc.supportsHdr10)
        assertTrue(hevc.isHardwareAccelerated)
        val avc = byMime.getValue(MediaFormat.MIMETYPE_VIDEO_AVC)
        assertFalse("an 8-bit-only decoder cannot play a 10-bit stream", avc.supports10Bit)
        assertFalse(avc.isHardwareAccelerated)
        assertFalse(profile.allowSoftwareDecode)
    }

    @Test
    fun `an hdr10 profile counts as both hdr10 and 10 bit`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.hevc.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_HEVC,
                profiles = intArrayOf(CodecProfileLevel.HEVCProfileMain10HDR10),
                hardware = true,
            ),
        )

        val hevc =
            DeviceProfiles
                .build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = true)
                .videoDecoders
                .single()

        assertTrue(hevc.supportsHdr10)
        assertTrue(hevc.supports10Bit)
        assertFalse(hevc.supportsDolbyVision)
    }

    @Test
    fun `an avc high decoder alone does not report 10 bit`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.avc.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_AVC,
                profiles = intArrayOf(CodecProfileLevel.AVCProfileHigh),
                hardware = true,
            ),
        )

        val avc =
            DeviceProfiles
                .build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = false)
                .videoDecoders
                .single()

        assertFalse("AVC High is 8-bit, whatever VP9 Profile 3 is numbered", avc.supports10Bit)
        assertFalse(avc.supportsHdr10)
        assertFalse(avc.supportsDolbyVision)
    }

    @Test
    fun `an avc high10 decoder reports 10 bit and not dolby vision`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.avc.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_AVC,
                profiles = intArrayOf(CodecProfileLevel.AVCProfileHigh10),
                hardware = true,
            ),
        )

        val avc =
            DeviceProfiles
                .build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = false)
                .videoDecoders
                .single()

        assertTrue("High10 is a 10-bit AVC profile", avc.supports10Bit)
        assertFalse("AVC High10 shares its number with DvheDtr, not its meaning", avc.supportsDolbyVision)
        assertFalse(avc.supportsHdr10)
    }

    @Test
    fun `vp9 profile 1 is 8 bit and av1 main10 hdr10 is hdr10`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.vp9.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_VP9,
                profiles = intArrayOf(CodecProfileLevel.VP9Profile0, CodecProfileLevel.VP9Profile1),
                hardware = true,
            ),
        )
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.av1.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_AV1,
                profiles = intArrayOf(CodecProfileLevel.AV1ProfileMain10HDR10),
                hardware = true,
            ),
        )

        val byMime =
            DeviceProfiles
                .build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = false)
                .videoDecoders
                .associateBy { it.mimeType }

        val vp9 = byMime.getValue(MediaFormat.MIMETYPE_VIDEO_VP9)
        assertFalse("VP9 Profile 1 shares its number with HEVC Main10", vp9.supports10Bit)
        assertFalse(vp9.supportsHdr10)
        val av1 = byMime.getValue(MediaFormat.MIMETYPE_VIDEO_AV1)
        assertTrue(av1.supportsHdr10)
        assertTrue("an HDR10 profile is a 10-bit one", av1.supports10Bit)
    }

    @Test
    fun `a dolby vision decoder reports dolby vision`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.dolby-vision.decoder",
                mime = MediaFormat.MIMETYPE_VIDEO_DOLBY_VISION,
                profiles = intArrayOf(CodecProfileLevel.DolbyVisionProfileDvheSt),
                hardware = true,
            ),
        )

        val dv =
            DeviceProfiles
                .build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = false)
                .videoDecoders
                .single()

        assertTrue(dv.supportsDolbyVision)
        assertFalse("Dolby Vision is not HDR10", dv.supportsHdr10)
    }

    @Test
    fun `an encoder is never a decoder`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.vendor.hevc.encoder",
                mime = MediaFormat.MIMETYPE_VIDEO_HEVC,
                profiles = intArrayOf(CodecProfileLevel.HEVCProfileMain10),
                hardware = true,
                encoder = true,
            ),
        )
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.android.aac.encoder",
                mime = MediaFormat.MIMETYPE_AUDIO_AAC,
                profiles = intArrayOf(),
                hardware = false,
                encoder = true,
            ),
        )

        val profile = DeviceProfiles.build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = false)

        assertTrue("an HEVC encoder decodes nothing", profile.videoDecoders.isEmpty())
        assertTrue(profile.audioDecoders.isEmpty())
    }

    @Test
    fun `an audio decoder reports no size or bitrate ceiling`() {
        ShadowMediaCodecList.addCodec(
            codec(
                name = "c2.android.aac.decoder",
                mime = MediaFormat.MIMETYPE_AUDIO_AAC,
                profiles = intArrayOf(),
                hardware = false,
            ),
        )

        val profile = DeviceProfiles.build(RuntimeEnvironment.getApplication(), allowSoftwareDecode = true)

        val aac = profile.audioDecoders.single()
        assertEquals(MediaFormat.MIMETYPE_AUDIO_AAC, aac.mimeType)
        assertEquals(null, aac.maxWidth)
        assertEquals(null, aac.maxBitrateBps)
        assertTrue("an audio decoder is not a video one", profile.videoDecoders.isEmpty())
        assertTrue(profile.allowSoftwareDecode)
    }

    private fun codec(
        name: String,
        mime: String,
        profiles: IntArray,
        hardware: Boolean,
        encoder: Boolean = false,
    ): MediaCodecInfo =
        MediaCodecInfoBuilder
            .newBuilder()
            .setName(name)
            .setIsEncoder(encoder)
            .setIsHardwareAccelerated(hardware)
            .setIsSoftwareOnly(!hardware)
            .setCapabilities(
                MediaCodecInfoBuilder.CodecCapabilitiesBuilder
                    .newBuilder()
                    .setMediaFormat(MediaFormat().apply { setString(MediaFormat.KEY_MIME, mime) })
                    .setIsEncoder(encoder)
                    .apply {
                        if (mime.startsWith("video/")) {
                            setColorFormats(intArrayOf(MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible))
                        }
                    }.setProfileLevels(
                        profiles
                            .map { profile ->
                                CodecProfileLevel().apply {
                                    this.profile = profile
                                    level = CodecProfileLevel.HEVCMainTierLevel41
                                }
                            }.toTypedArray(),
                    ).build(),
            ).build()
}
