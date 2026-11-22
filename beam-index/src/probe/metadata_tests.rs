use super::*;
use crate::probe::color::{
    ChromaLocation, ColorPrimaries, ColorRange, ColorSpace, ColorTransferCharacteristic,
};
use crate::probe::format::Disposition;

/// Matroska carries duration, bitrate, and frame counts as *tags* rather
/// than in the stream header, so for a large share of a real library the
/// header values are zero and these fallbacks are the only thing standing
/// between the UI and a file that claims to be 0 seconds long. That is
/// what the tests below are about.
fn tags(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

fn video_metadata(bit_rate: u64) -> VideoMetadata {
    VideoMetadata {
        bit_rate,
        max_rate: 0,
        delay: 0,
        width: 1920,
        height: 1080,
        format: PixelFormat::YUV420P,
        has_b_frames: false,
        aspect_ratio: Rational::new(1, 1),
        color_space: ColorSpace::BT709,
        color_range: ColorRange::MPEG,
        color_primaries: ColorPrimaries::BT709,
        color_transfer_characteristic: ColorTransferCharacteristic::BT709,
        chroma_location: ChromaLocation::Left,
        references: 0,
        intra_dc_precision: 0,
        codec_name: "h264".to_string(),
        profile: "High".to_string(),
        level: "4.0".to_string(),
    }
}

fn audio_metadata(bit_rate: u64, frames: usize, channels: u16) -> AudioMetadata {
    AudioMetadata {
        bit_rate,
        max_rate: 0,
        delay: 0,
        rate: 48_000,
        channels,
        format: SampleFormat::F32(crate::probe::format::SampleType::Planar),
        frames,
        align: 0,
        channel_layout: ChannelLayout {
            channels,
            description: None,
        },
        codec_name: "aac".to_string(),
        profile: "LC".to_string(),
        title: String::new(),
        language: String::new(),
    }
}

fn video_stream(
    duration: i64,
    frames: i64,
    metadata: HashMap<String, String>,
) -> VideoStreamMetadata {
    VideoStreamMetadata {
        index: 0,
        time_base: Rational::new(1, 1000),
        start_time: 0,
        duration,
        frames,
        rate: Some(Rational::new(24_000, 1001)),
        disposition: Disposition::default(),
        discard: Discard::Default,
        codec_id: CodecId::H264,
        video: video_metadata(0),
        metadata,
    }
}

fn subtitle_stream(duration: i64, metadata: HashMap<String, String>) -> SubtitleStreamMetadata {
    SubtitleStreamMetadata {
        index: 2,
        time_base: Rational::new(1, 1000),
        start_time: 0,
        duration,
        disposition: Disposition::default(),
        discard: Discard::Default,
        codec_id: CodecId::SUBRIP,
        codec_name: "subrip".to_string(),
        metadata,
    }
}

mod rationals {
    use super::*;

    #[test]
    fn a_valid_rational_is_converted_and_reduced() {
        // Time bases and frame rates arrive as raw numerator/denominator
        // pairs; every duration in the probe is computed from them.
        assert_eq!(
            into_rational(ffmpeg::Rational(1, 1000)),
            Ok(Ratio::new(1, 1000))
        );
        assert_eq!(
            into_rational(ffmpeg::Rational(24_000, 1001)),
            Ok(Ratio::new(24_000, 1001))
        );
        // `Ratio::new` reduces, so 2/4 and 1/2 are the same rational.
        assert_eq!(into_rational(ffmpeg::Rational(2, 4)), Ok(Ratio::new(1, 2)));
    }

    #[test]
    fn a_zero_denominator_is_reported_rather_than_dividing_by_zero() {
        // A corrupt or still-probing stream can report 1/0. `Ratio::new`
        // panics on a zero denominator, and a silent `0/1` default would
        // make every duration computed from it zero.
        assert_eq!(into_rational(ffmpeg::Rational(1, 0)), Err((1, 0)));
        assert_eq!(into_rational(ffmpeg::Rational(0, 0)), Err((0, 0)));
    }

    #[test]
    fn a_zero_numerator_is_a_valid_rational() {
        assert_eq!(
            into_rational(ffmpeg::Rational(0, 1000)),
            Ok(Ratio::new(0, 1000))
        );
    }
}

mod duration_strings {
    use super::*;

    #[test]
    fn a_matroska_duration_tag_is_parsed_to_seconds() {
        assert_eq!(
            parse_duration_string("00:45:23.000000000"),
            Some(45.0 * 60.0 + 23.0)
        );
        assert_eq!(parse_duration_string("01:00:00.5"), Some(3600.5));
        assert_eq!(parse_duration_string("00:00:00.000000000"), Some(0.0));
    }

    #[test]
    fn hours_are_not_capped_at_a_day() {
        // A concatenated recording can legitimately exceed 24 hours.
        assert_eq!(parse_duration_string("30:00:00.0"), Some(30.0 * 3600.0));
    }

    #[test]
    fn anything_that_is_not_three_colon_separated_numbers_is_rejected() {
        for bad in [
            "",
            "45:23",
            "00:45:23:00",
            "abc",
            "00:xx:23.0",
            "00:45:xx",
            "::",
        ] {
            assert_eq!(parse_duration_string(bad), None, "for {bad:?}");
        }
    }

    #[test]
    fn a_negative_or_non_finite_duration_is_rejected() {
        for bad in [
            "-01:00:00",
            "00:-05:00",
            "00:00:-1.5",
            "NaN:00:00",
            "00:nan:00",
            "00:00:inf",
            "-inf:00:00",
            "00:00:infinity",
            // Finite components whose total overflows.
            "1e308:00:00",
        ] {
            assert_eq!(parse_duration_string(bad), None, "for {bad:?}");
        }
    }

    proptest::proptest! {
        #[test]
        fn a_parsed_duration_is_finite_and_never_negative(
            hours in component(),
            minutes in component(),
            seconds in component(),
        ) {
            let tag = format!("{hours}:{minutes}:{seconds}");
            if let Some(duration) = parse_duration_string(&tag) {
                proptest::prop_assert!(
                    duration.is_finite() && duration >= 0.0,
                    "{tag:?} parsed to {duration}"
                );
            }
        }
    }

    /// One component of a duration tag: well-formed, negative, any
    /// float `f64` prints (`NaN`, `inf` and huge ones included), or the
    /// spellings `f64::parse` accepts beyond `Display`'s.
    fn component() -> impl proptest::strategy::Strategy<Value = String> {
        use proptest::prelude::*;
        prop_oneof![
            (0u32..100).prop_map(|value| format!("{value:02}")),
            any::<f64>().prop_map(|value| value.to_string()),
            prop::sample::select(vec!["NaN", "nan", "inf", "-inf", "infinity", "-0", "1e308"])
                .prop_map(str::to_string),
        ]
    }
}

mod bit_rate_fallback {
    use super::*;

    #[test]
    fn the_header_bitrate_wins_when_it_is_present() {
        let stream_tags = tags(&[("BPS", "999")]);
        assert_eq!(video_metadata(5_000).actual_bit_rate(&stream_tags), 5_000.0);
        assert_eq!(
            audio_metadata(320_000, 0, 2).actual_bit_rate(&stream_tags),
            320_000.0
        );
    }

    #[test]
    fn a_zero_header_bitrate_falls_back_to_the_bps_tag() {
        let stream_tags = tags(&[("BPS", "8000000")]);
        assert_eq!(video_metadata(0).actual_bit_rate(&stream_tags), 8_000_000.0);
        assert_eq!(
            audio_metadata(0, 0, 2).actual_bit_rate(&stream_tags),
            8_000_000.0
        );
    }

    #[test]
    fn a_malformed_bps_tag_yields_zero_rather_than_a_panic() {
        let stream_tags = tags(&[("BPS", "not-a-number")]);
        assert_eq!(video_metadata(0).actual_bit_rate(&stream_tags), 0.0);
        assert_eq!(audio_metadata(0, 0, 2).actual_bit_rate(&stream_tags), 0.0);
    }

    #[test]
    fn no_bitrate_anywhere_is_zero() {
        let none = HashMap::new();
        assert_eq!(video_metadata(0).actual_bit_rate(&none), 0.0);
        assert_eq!(audio_metadata(0, 0, 2).actual_bit_rate(&none), 0.0);
    }
}

mod frame_count_fallback {
    use super::*;

    #[test]
    fn the_header_frame_count_wins_when_it_is_present() {
        assert_eq!(
            video_stream(0, 1234, tags(&[("NUMBER_OF_FRAMES", "9")])).actual_frames(),
            1234
        );
        assert_eq!(
            audio_metadata(0, 1234, 2).actual_frames(&tags(&[("NUMBER_OF_FRAMES", "9")])),
            1234
        );
    }

    #[test]
    fn a_zero_frame_count_falls_back_to_the_tag() {
        assert_eq!(
            video_stream(0, 0, tags(&[("NUMBER_OF_FRAMES", "43200")])).actual_frames(),
            43_200
        );
        assert_eq!(
            audio_metadata(0, 0, 2).actual_frames(&tags(&[("NUMBER_OF_FRAMES", "43200")])),
            43_200
        );
    }

    #[test]
    fn a_malformed_or_missing_tag_yields_zero() {
        assert_eq!(
            video_stream(0, 0, tags(&[("NUMBER_OF_FRAMES", "lots")])).actual_frames(),
            0
        );
        assert_eq!(video_stream(0, 0, HashMap::new()).actual_frames(), 0);
        assert_eq!(audio_metadata(0, 0, 2).actual_frames(&HashMap::new()), 0);
    }
}

mod duration_fallback {
    use super::*;

    const FILE_DURATION: f64 = 7200.0;

    #[test]
    fn the_stream_duration_wins_when_it_is_present() {
        // 60_000 ticks at 1/1000 = 60 seconds.
        let stream = video_stream(60_000, 0, tags(&[("DURATION", "01:00:00.0")]));
        assert_eq!(stream.duration_seconds(), 60.0);
        assert_eq!(stream.actual_duration_seconds(FILE_DURATION), 60.0);
    }

    #[test]
    fn a_zero_stream_duration_falls_back_to_the_duration_tag() {
        let stream = video_stream(0, 0, tags(&[("DURATION", "00:02:30.0")]));
        assert_eq!(stream.actual_duration_seconds(FILE_DURATION), 150.0);

        let subtitles = subtitle_stream(0, tags(&[("DURATION", "00:02:30.0")]));
        assert_eq!(subtitles.actual_duration_seconds(FILE_DURATION), 150.0);
    }

    #[test]
    fn an_unparseable_duration_tag_falls_through_to_the_file_duration() {
        // Better a plausible container-level duration than a zero-length
        // track the player refuses to seek in.
        let stream = video_stream(0, 0, tags(&[("DURATION", "garbage")]));
        assert_eq!(stream.actual_duration_seconds(FILE_DURATION), FILE_DURATION);

        let subtitles = subtitle_stream(0, tags(&[("DURATION", "garbage")]));
        assert_eq!(
            subtitles.actual_duration_seconds(FILE_DURATION),
            FILE_DURATION
        );
    }

    #[test]
    fn no_duration_anywhere_falls_through_to_the_file_duration() {
        assert_eq!(
            video_stream(0, 0, HashMap::new()).actual_duration_seconds(FILE_DURATION),
            FILE_DURATION
        );
        assert_eq!(
            subtitle_stream(0, HashMap::new()).actual_duration_seconds(FILE_DURATION),
            FILE_DURATION
        );
    }
}

mod derived_descriptions {
    use super::*;

    #[test]
    fn channel_counts_map_to_the_layout_names_users_recognise() {
        assert_eq!(audio_metadata(0, 0, 1).channel_layout_description(), "Mono");
        assert_eq!(
            audio_metadata(0, 0, 2).channel_layout_description(),
            "Stereo"
        );
        assert_eq!(audio_metadata(0, 0, 6).channel_layout_description(), "5.1");
        assert_eq!(audio_metadata(0, 0, 8).channel_layout_description(), "7.1");
        // Anything else is described generically rather than guessed at.
        for channels in [0, 3, 4, 5, 7, 16] {
            assert_eq!(
                audio_metadata(0, 0, channels).channel_layout_description(),
                "Multi-channel",
                "for {channels} channels"
            );
        }
    }

    #[test]
    fn frame_rate_is_the_streams_rational_rate() {
        // 24000/1001 is NTSC film: a rational the probe must not round to
        // 24, or A/V sync drifts a frame every ~17 minutes.
        let stream = video_stream(0, 0, HashMap::new());
        let rate = stream.frame_rate().expect("a rate was probed");
        assert!((rate - 23.976_023_976).abs() < 1e-6, "got {rate}");
    }

    #[test]
    fn a_stream_without_a_rate_reports_none_rather_than_zero() {
        let mut stream = video_stream(0, 0, HashMap::new());
        stream.rate = None;
        assert_eq!(stream.frame_rate(), None);
    }

    /// A stream's bit depth is its pixel format's, as FFmpeg's own
    /// descriptor gives it -- 8-bit SDR, and the 10-, 12- and 16-bit
    /// formats HDR and mastering files use, in either byte order.
    #[test]
    fn video_bit_depth_comes_from_the_probed_format() {
        use ffmpeg::format::Pixel;
        for pixel in [
            Pixel::YUV420P,
            Pixel::YUV420P10LE,
            Pixel::YUV422P10BE,
            Pixel::GBRP10LE,
            Pixel::YUV420P12LE,
            Pixel::YUV444P12BE,
            Pixel::YUV420P16LE,
            Pixel::YUV444P16BE,
        ] {
            let mut video = video_metadata(0);
            video.format = PixelFormat::from(pixel);
            let descriptor = pixel.descriptor().expect("FFmpeg describes the format");
            // SAFETY: a descriptor points into FFmpeg's static table.
            let depth = unsafe { (*descriptor.as_ptr()).comp[0].depth };
            assert_eq!(
                video.bit_depth(),
                Some(u8::try_from(depth).unwrap()),
                "{pixel:?}"
            );
        }
    }
}

mod subtitle_tags {
    use super::*;

    #[test]
    fn title_and_language_are_read_from_the_stream_tags() {
        let subtitles = subtitle_stream(
            1000,
            tags(&[("title", "Forced (Alien)"), ("language", "eng")]),
        );
        assert_eq!(subtitles.title().as_deref(), Some("Forced (Alien)"));
        assert_eq!(subtitles.language().as_deref(), Some("eng"));
    }

    #[test]
    fn an_untagged_subtitle_track_reports_none_rather_than_an_empty_string() {
        // A `Some("")` would render as a blank entry in the track picker.
        let subtitles = subtitle_stream(1000, HashMap::new());
        assert_eq!(subtitles.title(), None);
        assert_eq!(subtitles.language(), None);
    }
}

mod chapters {
    use super::*;

    #[test]
    fn a_chapter_is_read_in_milliseconds_from_its_own_time_base() {
        // Matroska's nanosecond time base, MP4's millisecond one, and an
        // odd one that does not divide evenly.
        assert_eq!(
            chapter_metadata(
                90_000_000_000,
                180_500_000_000,
                (1, 1_000_000_000),
                Some("Opening")
            ),
            Some(ChapterMetadata {
                start_ms: 90_000,
                end_ms: 180_500,
                title: Some("Opening".into()),
            })
        );
        assert_eq!(
            chapter_metadata(1_500, 3_000, (1, 1_000), None),
            Some(ChapterMetadata {
                start_ms: 1_500,
                end_ms: 3_000,
                title: None,
            })
        );
        assert_eq!(
            chapter_metadata(10, 20, (1, 3), None).map(|c| (c.start_ms, c.end_ms)),
            Some((3_333, 6_666))
        );
    }

    #[test]
    fn a_blank_title_is_none_and_a_padded_one_is_trimmed() {
        let title = |raw| chapter_metadata(0, 1, (1, 1), Some(raw)).and_then(|c| c.title);
        assert_eq!(title("   "), None);
        assert_eq!(title(""), None);
        assert_eq!(title("  Ending  ").as_deref(), Some("Ending"));
    }

    #[test]
    fn a_chapter_before_the_start_or_in_no_time_base_is_dropped() {
        assert_eq!(chapter_metadata(-1, 10, (1, 1_000), None), None);
        assert_eq!(chapter_metadata(0, 10, (1, 0), None), None);
        assert_eq!(chapter_metadata(0, 10, (-1, 1_000), None), None);
    }

    #[test]
    fn a_chapter_ending_before_it_starts_ends_where_it_starts() {
        assert_eq!(
            chapter_metadata(5_000, 1_000, (1, 1_000), None).map(|c| (c.start_ms, c.end_ms)),
            Some((5_000, 5_000))
        );
        assert_eq!(
            chapter_metadata(5_000, -1, (1, 1_000), None).map(|c| (c.start_ms, c.end_ms)),
            Some((5_000, 5_000))
        );
    }

    proptest::proptest! {
        /// Whatever a container records, a chapter read never ends
        /// before it starts and never panics; one starting before zero,
        /// or in a time base that does not count forward, is not read.
        #[test]
        fn a_chapter_never_ends_before_it_starts(
            start in proptest::prelude::any::<i64>(),
            end in proptest::prelude::any::<i64>(),
            num in proptest::prelude::any::<i32>(),
            den in proptest::prelude::any::<i32>(),
        ) {
            let chapter = chapter_metadata(start, end, (num, den), None);
            if let Some(chapter) = &chapter {
                proptest::prop_assert!(chapter.end_ms >= chapter.start_ms, "{chapter:?}");
            }
            if start < 0 || num < 0 || den <= 0 {
                proptest::prop_assert!(chapter.is_none());
            }
        }

        /// In a millisecond time base a chapter is read as recorded.
        #[test]
        fn a_millisecond_chapter_is_read_as_recorded(
            start in 0i64..1_000_000_000,
            length in 0i64..1_000_000_000,
        ) {
            let chapter = chapter_metadata(start, start + length, (1, 1_000), None)
                .expect("a chapter from zero on");
            proptest::prop_assert_eq!(
                (chapter.start_ms, chapter.end_ms),
                (start as u64, (start + length) as u64)
            );
        }
    }
}
