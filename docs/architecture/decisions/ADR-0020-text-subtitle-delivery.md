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
UTF-8, UTF-16 (with or without a byte-order mark) or Windows-1252 file, rewriting each cue's timing,
starting a new cue at a timing line even where the blank line before it is missing, keeping `<b>`,
`<i>` and `<u>`,
dropping `<font>` and ASS override blocks, escaping the rest -- and normalises a WebVTT file to
UTF-8 with LF line ends. It is produced per request from the file on disk, never cached beside it,
and its validator derives from the file's modification time and length and the converter's
revision. A file over 8 MiB is not
converted; ASS and SSA are not converted at all, since their styling and positioning are what a
viewer chose them for and WebVTT cannot carry them. A track says whether it has a rendition by
carrying a `webvtt_url`.

**This is not transcoding.** ADR-0004 is about media: re-encoding or re-containering audio and
video, whose correctness depends on every codec a library holds and whose output is a new media
stream. Rewriting a subtitle file is parsing text with a pure function in `beam-domain`
(`utils::subtitle`), with no process, no FFmpeg, no decoder and no derived artifact; the boundary is
what is being rewritten, not how long it takes.

**A subtitle file is hostile input, and its cost is bounded by construction, not by assumption.**
Anyone who can drop a file into a library can hand Beam one, and the file can change between the
scan that recorded it and the request that reads it. So both operations:

- open the file beneath its library root with no symbolic link followed at any component below
  the root, the file or a folder above it, and with regular-file semantics
  (`beam_index::library_file::open_regular_file`: `openat2(RESOLVE_NO_SYMLINKS | RESOLVE_BENEATH)`
  on Linux, else an `openat` walk from the root with `O_NOFOLLOW` at every step; `O_NONBLOCK`; then
  refused unless the handle `fstat`s as a regular file), so a link out of the library at any level,
  or a FIFO or a device in the file's place, is `#source-file-missing` and never read (FR-212). The
  root itself is opened as configured, so a root that is itself a link is followed;
- serve length, modification time and bytes from that one handle, never from a second lookup of the
  path;
- for the rendition, read at most 8 MiB plus one byte, refusing the file when the handle's size or
  the bytes read exceed 8 MiB;
- convert in time linear in the input's length whatever it holds: a markup opener (`<`, `{\`) is
  closed only by a delimiter before the next opener of its kind, so unclosed markup costs one pass,
  not one pass per opener. A test converts 8 MiB lines of unclosed markup within a bound.

The rendition's validator names the converter's revision as well as the file, so a converter change
that alters the output is not answered `304` to a client holding the old bytes.

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
  well-formed WebVTT, linear in its input), with no infrastructure.

**Negative / accepted cost:**
- A browser cannot show an embedded subtitle stream, nor an ASS, SSA or image subtitle. The
  mitigation is the one ADR-0004 already gives: place a SubRip or WebVTT file beside the video.
- Conversion is per request, on a blocking worker. At 8 MiB the ceiling is about a hundred times a
  feature film's SubRip, and conversion is linear, so the worst file costs what an honest file of
  that size does -- a fraction of a second, not the minutes a quadratic scan took. A rendition
  carries a validator and `Cache-Control`, so a player re-fetching it revalidates rather than
  reconverts; an attacker can still request the rendition of a large file repeatedly, which costs
  what any authenticated request for 8 MiB does.

**Reversal:** an operator-facing cache of renditions, or conversion of ASS to WebVTT, would each be
an addition to this decision, not a reversal of it. Extracting embedded streams would reverse it,
and would need ADR-0004 superseded first.
