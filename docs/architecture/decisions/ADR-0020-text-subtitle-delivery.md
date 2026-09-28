# ADR-0020: Subtitle files are served, and SubRip rewritten as WebVTT; subtitle streams are not extracted

## Status

Accepted. Settles the subtitle half of [#189](https://github.com/justin13888/beam/issues/189).
Refines [ADR-0004](ADR-0004-never-transcode.md), whose boundary it draws more precisely rather than
moves.

## Context

A title's subtitles live in two places. Some are streams inside the video file -- SubRip, ASS,
`mov_text`, or an image format such as a Blu-ray's PGS (`hdmv_pgs_subtitle`) or a DVD's
VobSub. Others are text files beside it, `Movie.en.forced.srt`, which the indexer has recorded as
`sidecar_subtitles` rows since #184 and nothing served.

Clients differ in what they can read. Media3 and `AVPlayer` read SubRip, WebVTT and most embedded
text tracks themselves. A browser's `<track>` element reads WebVTT and nothing else -- and most
subtitle files in a library are SubRip.

[ADR-0004](ADR-0004-never-transcode.md) forbids transcoding and remuxing media at request time, and
the path it deleted included an `ffmpeg` shell-out specifically for subtitles. Any subtitle delivery
has to say which side of that line it is on.

## Decision

**A subtitle file beside a video is served as stored.**
`GET /v1/files/{file_id}/subtitles/{subtitle_id}` returns the file's bytes with its format's content
type (`application/x-subrip`, `text/vtt`, `text/x-ass`, `text/x-ssa`), Range-capable, through the
same byte source as file delivery. It is read-only: Beam never writes into a library root.

**A SubRip or WebVTT file is also served as WebVTT.**
`GET /v1/files/{file_id}/subtitles/{subtitle_id}/webvtt` rewrites SubRip as WebVTT -- decoding a
UTF-8, UTF-16 or Windows-1252 file, rewriting each cue's timing, keeping `<b>`, `<i>` and `<u>`,
dropping `<font>` and ASS override blocks, escaping the rest -- and normalises a WebVTT file to
UTF-8 with LF line ends. It is produced per request from the file on disk, never cached beside it,
and its validator derives from the file's modification time and length. A file over 8 MiB is not
converted; ASS and SSA are not converted at all, since their styling and positioning are what a
viewer chose them for and WebVTT cannot carry them. A track says whether it has a rendition by
carrying a `webvtt_url`.

**This is not transcoding.** ADR-0004 is about media: re-encoding or re-containering audio and
video, whose cost scales with a film's length and a server's concurrent viewers, and whose
correctness depends on every codec a library holds. A subtitle file is a few kilobytes of text
parsed by a pure function in `beam-domain` (`utils::subtitle`), with no process, no FFmpeg and no
derived artifact. It is closer to serving JSON than to encoding a frame.

**A subtitle stream inside the video is never extracted.** It is listed on its source as a track
with its stream index, codec and flags, and `is_text` saying whether a client can render it itself,
and no URL. Extracting it would mean demuxing the whole file to reach cues interleaved through it --
remuxing under another name, which ADR-0004 forbids. A client reads an embedded track from the
stream it is already playing, as Media3 and `AVPlayer` do. An image subtitle is never served in any
form.

## Consequences

**Positive:**
- A browser can show a library's most common subtitle files, and a native client can fetch any
  sidecar as the file it is.
- The rendition costs nothing to keep correct: nothing is stored, so there is nothing to invalidate
  when a subtitle file changes.
- The conversion is a pure function, table-tested and property-tested (never panics, always
  well-formed WebVTT), with no infrastructure.

**Negative / accepted cost:**
- A browser cannot show an embedded subtitle stream, nor an ASS, SSA or image subtitle. The
  mitigation is the one ADR-0004 already gives: place a SubRip or WebVTT file beside the video.
- Conversion is per request. At 8 MiB the ceiling is about a hundred times a feature film's SubRip,
  and a rendition carries a validator and `Cache-Control`, so a player re-fetching it revalidates
  rather than reconverts.

**Reversal:** an operator-facing cache of renditions, or conversion of ASS to WebVTT, would each be
an addition to this decision, not a reversal of it. Extracting embedded streams would reverse it,
and would need ADR-0004 superseded first.
