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
/// itself dropped. Unmarked bytes are UTF-16 when their NULs say so (see
/// [`unmarked_utf16`]), UTF-8 when they are valid UTF-8, and Windows-1252
/// otherwise -- the encoding a subtitle saved by an older Windows tool almost
/// always has, and one in which every byte decodes, so no input is refused.
/// Line ends become LF.
pub fn decode_subtitle_text(bytes: &[u8]) -> String {
    let text: Cow<'_, str> = if let Some(rest) = bytes.strip_prefix(b"\xEF\xBB\xBF") {
        String::from_utf8_lossy(rest)
    } else if let Some(rest) = bytes.strip_prefix(b"\xFF\xFE") {
        encoding_rs::UTF_16LE.decode_without_bom_handling(rest).0
    } else if let Some(rest) = bytes.strip_prefix(b"\xFE\xFF") {
        encoding_rs::UTF_16BE.decode_without_bom_handling(rest).0
    } else if let Some(encoding) = unmarked_utf16(bytes) {
        encoding.decode_without_bom_handling(bytes).0
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

/// The byte order of UTF-16 saved without a byte-order mark, or `None` when
/// `bytes` do not read as UTF-16.
///
/// Told by where the NULs in the first KiB fall. Subtitle text is largely
/// ASCII whatever its language -- the cue numbers, the timings, spaces and
/// punctuation -- and UTF-16 writes each ASCII character as its byte beside a
/// NUL: after it in little-endian, before it in big-endian. UTF-8 and
/// Windows-1252 text hold no NUL at all. A quarter of the code units sampled
/// must carry a NUL on one side, and NULs there must outnumber those on the
/// other side four to one, which the zero low byte of a CJK character such as
/// U+4E00 cannot tip.
fn unmarked_utf16(bytes: &[u8]) -> Option<&'static encoding_rs::Encoding> {
    const SAMPLE_BYTES: usize = 1024;
    let sample = &bytes[..bytes.len().min(SAMPLE_BYTES) & !1];
    let units = sample.len() / 2;
    let (mut first_nul, mut second_nul) = (0_usize, 0_usize);
    for unit in sample.chunks_exact(2) {
        first_nul += usize::from(unit[0] == 0);
        second_nul += usize::from(unit[1] == 0);
    }
    let reads_as = |ascii_side: usize, other_side: usize| {
        ascii_side > 0 && ascii_side * 4 >= units && ascii_side > other_side * 4
    };
    if reads_as(second_nul, first_nul) {
        Some(encoding_rs::UTF_16LE)
    } else if reads_as(first_nul, second_nul) {
        Some(encoding_rs::UTF_16BE)
    } else {
        None
    }
}

/// Rewrite a SubRip file as WebVTT.
///
/// Each blank-line-separated block is a cue: an optional numeric id, a timing
/// line `H:MM:SS,mmm --> H:MM:SS,mmm`, and its text. A timing line inside a
/// cue's text -- a file missing the blank line between two cues -- starts the
/// next cue, taking the numeric id line before it along. A block whose timing
/// cannot be read is dropped and counted in [`SrtConversion::skipped`], so
/// one damaged cue costs that cue and not the file.
///
/// Linear in the input's length whatever it holds, since a file in a library
/// root is not trusted: see `cue_text`.
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
/// whitespace separates blocks, as an empty one does, and so does a timing
/// line past the place a block's own timing takes: it starts the next block,
/// with the numeric id line before it.
fn blocks(text: &str) -> Vec<Vec<&str>> {
    let mut blocks = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.split('\n') {
        if line.trim().is_empty() {
            if !current.is_empty() {
                blocks.push(std::mem::take(&mut current));
            }
            continue;
        }
        // A block's timing is its first line, or its second after an id --
        // where `cue` reads it. A timing line past that place is not text.
        let timing_place_passed = match current.as_slice() {
            [] => false,
            [first] => first.contains("-->"),
            [_, _, ..] => true,
        };
        if timing_place_passed && timing(line).is_some() {
            let next_id = match current.as_slice() {
                [_, .., last] if is_numeric_id(last) => current.pop(),
                _ => None,
            };
            blocks.push(std::mem::take(&mut current));
            current.extend(next_id);
        }
        current.push(line);
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

/// Whether `line` is a SubRip cue number.
fn is_numeric_id(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && line.bytes().all(|b| b.is_ascii_digit())
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
    if let Some(id) = id
        && is_numeric_id(id)
    {
        cue.push_str(id.trim());
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
///
/// Linear in the line's length: an opener is closed only by a delimiter
/// before the next opener of its kind (see [`closer`]), so a line of
/// unmatched `<` or `{\` -- which a hostile file can make megabytes long --
/// costs one pass over it, not one pass per opener.
fn cue_text(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(c) = rest.chars().next() {
        if c == '{'
            && rest[1..].starts_with('\\')
            && let Some(close) = closer(rest, b'{', b'}')
        {
            // An ASS override block: styling WebVTT cannot express.
            rest = &rest[close + 1..];
            continue;
        }
        if c == '<'
            && let Some(close) = closer(rest, b'<', b'>')
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

/// The byte offset of the `close` that ends the markup `rest` starts with,
/// when one comes before the next `open`; an opener with none is text.
///
/// Stopping at the next opener is what keeps [`cue_text`] linear: the bytes
/// between two openers of a kind are scanned once, from the first of them,
/// and a scan that fails leaves the next opener to begin its own. Both
/// delimiters are ASCII, so the offset is a character boundary.
fn closer(rest: &str, open: u8, close: u8) -> Option<usize> {
    let bytes = rest.as_bytes();
    let found = bytes
        .iter()
        .skip(1)
        .position(|&b| b == open || b == close)?
        + 1;
    (bytes[found] == close).then_some(found)
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
            (
                "no blank line between cues",
                b"1\n00:00:01,000 --> 00:00:02,000\nHi\n2\n00:00:03,000 --> 00:00:04,000\nBye\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nHi\n\n2\n00:00:03.000 --> 00:00:04.000\nBye\n",
            ),
            (
                "no blank line between cues without ids",
                b"00:00:01,000 --> 00:00:02,000\nHi\n00:00:03,000 --> 00:00:04,000\nBye\n",
                "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n\n00:00:03.000 --> 00:00:04.000\nBye\n",
            ),
            (
                "a cue with no text run into the next",
                b"1\n00:00:01,000 --> 00:00:02,000\n2\n00:00:03,000 --> 00:00:04,000\nBye\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\n\n2\n00:00:03.000 --> 00:00:04.000\nBye\n",
            ),
            (
                "an unreadable timing in the text stays text",
                b"1\n00:00:01,000 --> 00:00:02,000\nA --> B\n00:00:61,000 --> 00:00:62,000\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\nA --&gt; B\n00:00:61,000 --&gt; 00:00:62,000\n",
            ),
            (
                "an unclosed tag before a kept one",
                b"1\n00:00:01,000 --> 00:00:02,000\n<font <b>bold</b>\n",
                "WEBVTT\n\n1\n00:00:01.000 --> 00:00:02.000\n&lt;font <b>bold</b>\n",
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

    /// UTF-16 saved without a byte-order mark, as some Windows tools write
    /// it, is read by where its NULs fall -- text in Latin and in CJK script
    /// alike, whose characters put a NUL on the other side too.
    #[test]
    fn utf16_without_a_byte_order_mark_is_read() {
        for text in [
            "1\n00:00:01,000 --> 00:00:02,000\nCaf\u{e9}\n",
            "1\n00:00:01,000 --> 00:00:02,000\n\u{4e00}\u{4e8c}\u{4e09}\u{65e5}\u{672c}\u{8a9e}\n",
        ] {
            let little: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let big: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
            let expected = srt_to_webvtt(text.as_bytes()).vtt;
            assert_eq!(expected.lines().count(), 5, "{text:?} converts to a cue");
            assert_eq!(srt_to_webvtt(&little).vtt, expected, "{text:?} as UTF-16LE");
            assert_eq!(srt_to_webvtt(&big).vtt, expected, "{text:?} as UTF-16BE");
        }
    }

    /// A timing line in a block that already has one is the next cue's,
    /// the stray lines before it one skipped block.
    #[test]
    fn a_timing_line_past_a_blocks_own_starts_the_next_cue() {
        let converted = srt_to_webvtt(
            b"junk\nmore junk\n00:00:01,000 --> 00:00:02,000\nHi\n3\n00:00:03,000 --> 00:00:04,000\nBye\n",
        );
        assert_eq!((converted.cues, converted.skipped), (2, 1));
        assert_eq!(
            converted.vtt,
            "WEBVTT\n\n00:00:01.000 --> 00:00:02.000\nHi\n\n3\n00:00:03.000 --> 00:00:04.000\nBye\n"
        );
    }

    /// The conversion is linear however many markup openers go unclosed: an
    /// 8 MiB line of them -- the most the server converts -- takes well
    /// under the bound even unoptimised, where scanning the rest of the line
    /// at each opener took about half an hour.
    #[test]
    fn a_line_of_unclosed_markup_converts_in_linear_time() {
        const LINE_BYTES: usize = 8 * 1024 * 1024;
        let shapes: &[(&str, &str, &str)] = &[
            ("unclosed tags", "<", "&lt;"),
            ("unclosed overrides", "{\\", "{\\"),
            ("both, with text", "<a{\\b", "&lt;a{\\b"),
        ];
        for (name, unit, escaped) in shapes {
            let line = unit.repeat(LINE_BYTES / unit.len());
            let srt = format!("1\n00:00:01,000 --> 00:00:02,000\n{line}\n");
            let started = std::time::Instant::now();
            let converted = srt_to_webvtt(srt.as_bytes());
            let took = started.elapsed();
            assert!(took < std::time::Duration::from_secs(5), "{name}: {took:?}");
            assert_eq!(converted.cues, 1, "{name}");
            let expected_text = escaped.repeat(LINE_BYTES / unit.len());
            assert!(
                converted.vtt.ends_with(&format!(".000\n{expected_text}\n")),
                "{name}: the unclosed markup is text"
            );
        }
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

        /// A line with no closing `>` or `}` has no markup to keep or drop,
        /// however many openers it holds: it is escaped, character for
        /// character, and nothing else. Long runs of openers are likely.
        #[test]
        fn a_line_with_no_closer_is_only_escaped(
            parts in prop::collection::vec(
                prop_oneof![
                    Just("<".repeat(64)),
                    Just("{\\".repeat(32)),
                    Just("<".to_string()),
                    Just("{\\".to_string()),
                    Just("{".to_string()),
                    Just("&".to_string()),
                    Just("-".to_string()),
                    "[a-z /]{1,8}",
                    any::<char>()
                        .prop_filter("no closer", |c| !matches!(c, '>' | '}'))
                        .prop_map(String::from),
                ],
                0..256,
            ),
        ) {
            let line = parts.concat();
            let escaped = line.replace('&', "&amp;").replace('<', "&lt;");
            prop_assert_eq!(cue_text(&line), escaped);
        }

        /// A file missing the blank lines between its cues converts to the
        /// same WebVTT as one that has them.
        #[test]
        fn cues_without_blank_lines_between_them_are_still_cues(
            cues in prop::collection::vec(
                (
                    any::<bool>(),
                    0_u64..360_000_000,
                    0_u64..60_000,
                    prop::collection::vec("[A-Za-z][A-Za-z ,.!?']{0,20}", 0..3),
                ),
                1..8,
            ),
        ) {
            let written: Vec<String> = cues
                .iter()
                .enumerate()
                .map(|(index, (numbered, start, length, text))| {
                    let srt_time = |millis: u64| timestamp(millis).replace('.', ",");
                    let id = if *numbered { format!("{}\n", index + 1) } else { String::new() };
                    let mut cue = format!("{id}{} --> {}", srt_time(*start), srt_time(start + length));
                    for line in text {
                        cue.push('\n');
                        cue.push_str(line);
                    }
                    cue
                })
                .collect();
            let separated = srt_to_webvtt(written.join("\n\n").as_bytes());
            let run_together = srt_to_webvtt(written.join("\n").as_bytes());
            prop_assert_eq!(separated.cues, cues.len());
            prop_assert_eq!(run_together, separated);
        }
    }
}
