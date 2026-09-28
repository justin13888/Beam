//! Text subtitles as a browser reads them (issue #189).
//!
//! A browser's `<track>` element reads WebVTT and nothing else, and most text
//! subtitles in a library are SubRip (`.srt`). So Beam offers every SubRip
//! and WebVTT sidecar a second time as WebVTT: SubRip rewritten, WebVTT
//! normalised to UTF-8 with LF line ends. This is a rewrite of a few kilobytes
//! of text, not of media -- ADR-0004 forbids transcoding and remuxing audio
//! and video, and ADR-0020 records why this falls outside it.
//!
//! Pure: bytes in, text out. Nothing here reads a file.

use std::borrow::Cow;

/// The FFmpeg codec names of the subtitle formats that are text, which a
/// client can render itself without decoding a bitmap.
///
/// Every other subtitle codec -- `hdmv_pgs_subtitle`, `dvd_subtitle`,
/// `dvb_subtitle`, `xsub` and the teletext and caption streams -- is an image
/// or a broadcast signal a client must decode as it plays.
const TEXT_SUBTITLE_CODECS: &[&str] = &[
    "subrip",
    "srt",
    "webvtt",
    "ass",
    "ssa",
    "mov_text",
    "text",
    "ttml",
    "microdvd",
    "mpl2",
    "jacosub",
    "pjs",
    "realtext",
    "sami",
    "stl",
    "subviewer",
    "subviewer1",
    "vplayer",
];

/// Whether the subtitle codec FFmpeg names `codec` is text rather than an
/// image. Compared ignoring ASCII case.
pub fn is_text_subtitle_codec(codec: &str) -> bool {
    TEXT_SUBTITLE_CODECS
        .iter()
        .any(|text| text.eq_ignore_ascii_case(codec))
}

/// A SubRip file rewritten as WebVTT.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SrtConversion {
    /// The WebVTT document: always starts `WEBVTT` and ends with a newline.
    pub vtt: String,
    /// How many cues it carries.
    pub cues: usize,
    /// How many non-empty blocks of the SubRip were dropped because no cue
    /// timing could be read from them.
    pub skipped: usize,
}

/// Read subtitle bytes as text, whatever encoding they were saved in.
///
/// A byte-order mark decides first: UTF-8, UTF-16LE or UTF-16BE, the mark
/// itself dropped. Unmarked bytes are UTF-8 when they are valid UTF-8 and
/// Windows-1252 otherwise -- the encoding a subtitle saved by an older Windows
/// tool almost always has, and one in which every byte decodes, so no input is
/// refused. Line ends become LF.
pub fn decode_subtitle_text(bytes: &[u8]) -> String {
    let text: Cow<'_, str> = if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        String::from_utf8_lossy(rest)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        encoding_rs::UTF_16LE.decode_without_bom_handling(rest).0
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        encoding_rs::UTF_16BE.decode_without_bom_handling(rest).0
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => Cow::Borrowed(text),
            Err(_) => {
                encoding_rs::WINDOWS_1252
                    .decode_without_bom_handling(bytes)
                    .0
            }
        }
    };
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Rewrite a SubRip file as WebVTT.
///
/// Each blank-line-separated block is a cue: an optional numeric id, a timing
/// line `H:MM:SS,mmm --> H:MM:SS,mmm`, and its text. A block whose timing
/// cannot be read is dropped and counted in [`SrtConversion::skipped`], so
/// one damaged cue costs that cue and not the file.
///
/// The timing is rewritten with a `.` before the milliseconds and at least
/// two hour digits; SubRip's `X1:` position coordinates are dropped. In the
/// text, `<b>`, `<i>` and `<u>` are kept, `<font>` and ASS override blocks
/// (`{\an8}`) are removed, and every other `&` and `<` is escaped -- WebVTT
/// reads both as markup. A `-->` in the text is escaped too, since it would
/// read as a timing line, and a line left empty is dropped, since a blank line
/// would end the cue.
pub fn srt_to_webvtt(bytes: &[u8]) -> SrtConversion {
    let text = decode_subtitle_text(bytes);
    let mut vtt = String::from("WEBVTT\n");
    let mut cues = 0;
    let mut skipped = 0;
    for block in blocks(&text) {
        match cue(&block) {
            Some(cue) => {
                vtt.push('\n');
                vtt.push_str(&cue);
                cues += 1;
            }
            None => skipped += 1,
        }
    }
    SrtConversion { vtt, cues, skipped }
}

/// Bring a WebVTT file to the form [`srt_to_webvtt`] writes: UTF-8, LF line
/// ends, a `WEBVTT` header, and a final newline. The cues are left as they
/// are: a file that is already WebVTT is served, not reinterpreted.
pub fn normalize_webvtt(bytes: &[u8]) -> String {
    let mut text = decode_subtitle_text(bytes);
    // The header is `WEBVTT` alone or followed by a space or tab and a
    // comment; anything else means the header is missing.
    let first_line = text.lines().next().unwrap_or_default();
    let has_header = first_line
        .strip_prefix("WEBVTT")
        .is_some_and(|rest| rest.is_empty() || rest.starts_with([' ', '\t']));
    if !has_header {
        text.insert_str(0, "WEBVTT\n\n");
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// The non-empty blocks of `text`, each as its lines. A line of nothing but
/// whitespace separates blocks, as an empty one does.
fn blocks(text: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
        } else {
            current.push(line);
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// One block as a WebVTT cue, or `None` when it has no readable timing.
fn cue(block: &[&str]) -> Option<String> {
    // The timing is the first line, or the second after an id.
    let (id, timing_line, text) = match block {
        [first, rest @ ..] if first.contains("-->") => (None, *first, rest),
        [first, second, rest @ ..] if second.contains("-->") => (Some(*first), *second, rest),
        _ => return None,
    };
    let (start, end) = timing(timing_line)?;

    let mut cue = String::new();
    // Only a numeric id is kept: it is what SubRip writes, and anything else
    // in that position is more likely stray text than an identifier.
    if let Some(id) = id.map(str::trim)
        && !id.is_empty()
        && id.bytes().all(|b| b.is_ascii_digit())
    {
        cue.push_str(id);
        cue.push('\n');
    }
    cue.push_str(&format!("{} --> {}\n", timestamp(start), timestamp(end)));
    for line in text {
        let line = cue_text(line);
        if !line.trim().is_empty() {
            cue.push_str(&line);
            cue.push('\n');
        }
    }
    Some(cue)
}

/// The start and end of a timing line, in milliseconds. Whatever follows the
/// end time -- SubRip's `X1:` coordinates -- is ignored. An end before the
/// start is not a cue.
fn timing(line: &str) -> Option<(u64, u64)> {
    let (start, rest) = line.split_once("-->")?;
    let end = rest.split_whitespace().next()?;
    let (start, end) = (parse_timestamp(start.trim())?, parse_timestamp(end)?);
    (end >= start).then_some((start, end))
}

/// `H:MM:SS,mmm` (or with a `.`) in milliseconds. Hours may run to three
/// digits; minutes and seconds to two, each below 60; the fraction to three
/// digits, read as a fraction (`,5` is half a second).
fn parse_timestamp(text: &str) -> Option<u64> {
    let (clock, fraction) = text.split_once([',', '.'])?;
    let mut parts = clock.split(':');
    let (hours, minutes, seconds) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let number = |digits: &str, max_len: usize| -> Option<u64> {
        (!digits.is_empty()
            && digits.len() <= max_len
            && digits.bytes().all(|b| b.is_ascii_digit()))
        .then(|| digits.parse().ok())
        .flatten()
    };
    let hours = number(hours, 3)?;
    let minutes = number(minutes, 2).filter(|m| *m < 60)?;
    let seconds = number(seconds, 2).filter(|s| *s < 60)?;
    let millis = number(fraction, 3)? * 10_u64.pow(3 - fraction.len() as u32);
    Some(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

/// Milliseconds as WebVTT's `HH:MM:SS.mmm`, the hours at least two digits.
fn timestamp(millis: u64) -> String {
    let (seconds, millis) = (millis / 1000, millis % 1000);
    let (minutes, seconds) = (seconds / 60, seconds % 60);
    let (hours, minutes) = (minutes / 60, minutes % 60);
    format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}")
}

/// The tags WebVTT and SubRip share, kept as they are.
const KEPT_TAGS: &[&str] = &["b", "i", "u"];

/// One line of cue text in WebVTT's syntax.
fn cue_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '{'
            && rest[1..].starts_with('\\')
            && let Some(close) = rest.find('}')
        {
            // An ASS override block: styling WebVTT cannot express.
            rest = &rest[close + 1..];
            continue;
        }
        if c == '<'
            && let Some(close) = rest.find('>')
        {
            let tag = &rest[1..close];
            let name = tag.strip_prefix('/').unwrap_or(tag).trim();
            if KEPT_TAGS.iter().any(|kept| kept.eq_ignore_ascii_case(name)) {
                let slash = if tag.starts_with('/') { "/" } else { "" };
                out.push_str(&format!("<{slash}{}>", name.to_ascii_lowercase()));
                rest = &rest[close + 1..];
                continue;
            }
            let bare = name.split_whitespace().next().unwrap_or_default();
            if bare.eq_ignore_ascii_case("font") {
                rest = &rest[close + 1..];
                continue;
            }
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            other => out.push(other),
        }
        rest = &rest[c.len_utf8()..];
    }
    out.replace("-->", "--&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn a_subrip_file_becomes_webvtt() {
        let cases: &[(&str, &[u8], &str)] = &[
            (
                "basic",
                b"1\n00:00:01,000 --> 00:00:02,500\nHello\nthere\n\n2\n00:00:03,000 --> 00:00:04,000\nBye\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.500\nHello\nthere\n\n2\n00:00:03.000 --> 00:00:04.000\nBye\n",
            ),
            (
                "CRLF line ends",
                b"1\r\n00:00:01,000 --> 00:00:02,000\r\nHi\r\n\r\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nHi\n",
            ),
            (
                "a UTF-8 byte-order mark",
                b"\xEF\xBB\xBF1\n00:00:01,000 --> 00:00:02,000\nHi\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nHi\n",
            ),
            (
                "Windows-1252",
                b"1\n00:00:01,000 --> 00:00:02,000\nCaf\xE9\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nCaf\u{e9}\n",
            ),
            (
                "one-digit hours and a dot",
                b"7\n1:02:03.004 --> 1:02:04.5\nLate\n",
                "WEBVTT\n\n7\n01:02:03.004 --> 01:02:04.500\nLate\n",
            ),
            (
                "three-digit hours",
                b"00:00:00,000 --> 100:00:00,000\nLong\n",
                "WEBVTT\n\n00:00:00.000 --> 100:00:00.000\nLong\n",
            ),
            (
                "position coordinates",
                b"1\n00:00:01,000 --> 00:00:02,000 X1:100 X2:200 Y1:10 Y2:20\nHi\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nHi\n",
            ),
            (
                "no id",
                b"00:00:01,000 --> 00:00:02,000\nHi\n",
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n",
            ),
            (
                "a non-numeric id",
                b"intro\n00:00:01,000 --> 00:00:02,000\nHi\n",
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n",
            ),
            (
                "font tags",
                b"1\n00:00:01,000 --> 00:00:02,000\n<font color=\"#ff0000\">Red</font> <B>bold</B>\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nRed <b>bold</b>\n",
            ),
            (
                "an ASS override",
                b"1\n00:00:01,000 --> 00:00:02,000\n{\\an8}Top\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nTop\n",
            ),
            (
                "a line that is only an override",
                b"1\n00:00:01,000 --> 00:00:02,000\n{\\an8}\nText\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nText\n",
            ),
            (
                "an arrow in the text",
                b"1\n00:00:01,000 --> 00:00:02,000\nA --> B\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nA --&gt; B\n",
            ),
            (
                "markup characters",
                b"1\n00:00:01,000 --> 00:00:02,000\nTom & Jerry <3 {not ass}\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nTom &amp; Jerry &lt;3 {not ass}\n",
            ),
            ("empty", b"", "WEBVTT\n"),
            ("only blank lines", b"\n\n \n", "WEBVTT\n"),
        ];
        for (name, srt, expected) in cases {
            let converted = srt_to_webvtt(srt);
            assert_eq!(converted.vtt, *expected, "{name}");
            assert_eq!(converted.skipped, 0, "{name}");
        }
    }

    #[test]
    fn utf16_with_a_byte_order_mark_is_read() {
        let text = "1\n00:00:01,000 --> 00:00:02,000\nCaf\u{e9} \u{65e5}\u{672c}\n";
        let little: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        let big: Vec<u8> = [0xFE, 0xFF]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let expected = "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nCaf\u{e9} \u{65e5}\u{672c}\n";
        assert_eq!(srt_to_webvtt(&little).vtt, expected);
        assert_eq!(srt_to_webvtt(&big).vtt, expected);
    }

    #[test]
    fn a_block_with_no_readable_timing_is_skipped_and_counted() {
        let srt = b"1\n00:00:01,000 --> 00:00:02,000\nKept\n\n\
                    2\nnot a timing\nLost\n\n\
                    3\n00:00:61,000 --> 00:00:62,000\nSixty-one seconds\n\n\
                    4\n00:00:05,000 --> 00:00:04,000\nEnds before it starts\n\n\
                    5\n00:00:07 --> 00:00:08\nNo fraction\n\n\
                    stray text\n";
        let converted = srt_to_webvtt(srt);
        assert_eq!(converted.cues, 1);
        assert_eq!(converted.skipped, 5);
        assert_eq!(
            converted.vtt,
            "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nKept\n"
        );
    }

    #[test]
    fn webvtt_is_normalised_but_not_reinterpreted() {
        let cases: &[(&str, &[u8], &str)] = &[
            (
                "already clean",
                b"WEBVTT\n\n00:01.000 --> 00:02.000\n<v Bob>Hi\n",
                "WEBVTT\n\n00:01.000 --> 00:02.000\n<v Bob>Hi\n",
            ),
            (
                "a byte-order mark, CRLF, and no final newline",
                b"\xEF\xBB\xBFWEBVTT - comment\r\n\r\n00:01.000 --> 00:02.000\r\nHi",
                "WEBVTT - comment\n\n00:01.000 --> 00:02.000\nHi\n",
            ),
            (
                "no header",
                b"00:01.000 --> 00:02.000\nHi\n",
                "WEBVTT\n\n00:01.000 --> 00:02.000\nHi\n",
            ),
            (
                "a header that is only a prefix",
                b"WEBVTTX\n",
                "WEBVTT\n\nWEBVTTX\n",
            ),
        ];
        for (name, input, expected) in cases {
            assert_eq!(normalize_webvtt(input), *expected, "{name}");
        }
    }

    #[test]
    fn text_and_image_subtitle_codecs_are_told_apart() {
        for text in ["subrip", "SubRip", "ass", "ssa", "webvtt", "mov_text"] {
            assert!(is_text_subtitle_codec(text), "{text}");
        }
        for image in [
            "hdmv_pgs_subtitle",
            "dvd_subtitle",
            "dvb_subtitle",
            "xsub",
            "dvb_teletext",
            "",
        ] {
            assert!(!is_text_subtitle_codec(image), "{image}");
        }
    }

    /// A line of SubRip-ish text: arbitrary characters, with the ones the
    /// converter treats specially made likely.
    fn line() -> impl Strategy<Value = String> {
        prop::collection::vec(
            prop_oneof![
                Just("-->".to_string()),
                Just("<b>".to_string()),
                Just("</i>".to_string()),
                Just("<font color=red>".to_string()),
                Just("{\\an8}".to_string()),
                Just("&".to_string()),
                Just("<".to_string()),
                Just("\r".to_string()),
                Just("\n".to_string()),
                Just(" ".to_string()),
                Just("00:00:01,000 --> 00:00:02,000".to_string()),
                Just("12".to_string()),
                any::<char>().prop_map(String::from),
            ],
            0..12,
        )
        .prop_map(|parts| parts.concat())
    }

    proptest! {
        /// Whatever the input, the output is WebVTT a parser reads cue by
        /// cue: the header first, `-->` only on timing lines, no blank line
        /// inside a cue, and every non-empty block either a cue or counted
        /// as skipped.
        #[test]
        fn any_input_converts_to_well_formed_webvtt(
            lines in prop::collection::vec(line(), 0..16),
            raw in prop::collection::vec(any::<u8>(), 0..64),
        ) {
            for input in [lines.join("\n").into_bytes(), raw] {
                let SrtConversion { vtt, cues, skipped } = srt_to_webvtt(&input);
                prop_assert!(vtt.starts_with("WEBVTT\n"));
                prop_assert!(vtt.ends_with('\n'));

                let blocks = blocks(&decode_subtitle_text(&input)).len();
                prop_assert_eq!(cues + skipped, blocks);

                let body = &vtt["WEBVTT\n".len()..];
                let written: Vec<&str> = body
                    .split("\n\n")
                    .map(|cue| cue.trim_matches('\n'))
                    .filter(|cue| !cue.is_empty())
                    .collect();
                prop_assert_eq!(written.len(), cues);
                for cue in written {
                    let lines: Vec<&str> = cue.split('\n').collect();
                    let timing_at = lines
                        .iter()
                        .position(|line| line.contains("-->"))
                        .expect("every cue has a timing line");
                    prop_assert!(timing_at <= 1);
                    for line in &lines[timing_at + 1..] {
                        prop_assert!(!line.contains("-->"));
                        prop_assert!(!line.trim().is_empty());
                    }
                }
            }
        }
    }
}
