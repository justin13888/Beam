# Streaming & Delivery

`beam-server` never transcodes, remuxes, or re-encodes media at request time — it serves the bytes
of whichever file the request resolves to, and nothing else. There is no ffmpeg anywhere in the
request path; the only ffmpeg usage in the workspace is `beam-index`'s metadata probing at index
time, which populates `media_streams`. See [ADR-0004](decisions/ADR-0004-never-transcode.md) for
the rationale and accepted trade-offs. Adaptive-bitrate streaming (HLS/DASH) is not deferred, it is
rejected — see [ADR-0014](decisions/ADR-0014-adaptive-streaming-rejected.md), which also records the
client-side work that answers the compatibility and bandwidth problems ABR would have been reached
for.

## The three delivery scenarios

Every media request resolves to exactly one of these; the API surface and client UI are organized
around them.

| Scenario | Trigger | Endpoint | Behavior |
|---|---|---|---|
| (a) Full download | User explicitly requests a file for offline use | `GET /v1/files/{fileId}/download` | Streams the file as-is with `Content-Disposition: attachment; filename="…"` (sanitized original name). Range-capable, so downloads are resumable. |
| (b) Direct-play streaming (the default) | User presses play | `GET /v1/files/{fileId}/stream` | Serves byte ranges inline as the player (Vidstack in `beam-web`) seeks/buffers; the browser's native decoding handles the source container/codec. |
| (c) Source-quality selection | User (or client, on observed network conditions) picks a different existing version of the title | Same `/stream` endpoint, different `fileId` | A full direct-play against a smaller file that already exists on disk — never a server-generated bitrate. |

For (c): a logical title (`movie_entries` row or `episodes` row) can have multiple `files` rows —
e.g. a 1080p remux and a separately indexed 480p re-encode. `GET /v1/media/{id}/sources` exposes
the available versions with size, container, duration, edition, and every video, audio and
subtitle track by stream index with FFmpeg's own codec name (`h264`, `hevc`, `eac3`, `truehd`,
`subrip`, `hdmv_pgs_subtitle`), plus each file's `stream_url` and `download_url`. It lists them
primary first -- the default edition, then the tallest picture, the highest video bit rate, the
largest file -- ranked when read rather than stored, and marks that one `is_primary`; the detail
route's `file_id` is the same file ([#189](https://github.com/justin13888/beam/issues/189)).
Selecting one is just a request against that file's own `/stream` endpoint. Sources
accept a movie id or an episode id; a show id is rejected with 400, since shows have no files of
their own. Episode sources landed in
[#102](https://github.com/justin13888/beam/pull/102), closing
[#68](https://github.com/justin13888/beam/issues/68). If an operator wants a low-bandwidth option,
they place a second, smaller rip in the library and let it get indexed — Beam does not create it.

That instruction holds for television too. The indexer find-or-creates an episode by
`(season_id, episode_number)` (`ShowRepository::find_or_create_episode`, an atomic
`INSERT ... ON CONFLICT DO NOTHING` on `idx_episodes_unique`), so a second file for an episode
attaches to the existing row as another source rather than failing the scan -- closed by
[#142](https://github.com/justin13888/beam/issues/142), the condition
[ADR-0014](decisions/ADR-0014-adaptive-streaming-rejected.md) set for the no-adaptive-streaming
decision. The existing episode is never written on that path: its title and runtime stay those the
first file (or enrichment) set.

"Observed network conditions" in (c) are now something a server can count. With playback telemetry
enabled (FR-511, [ADR-0019](decisions/ADR-0019-telemetry-posture.md)), clients report mid-stream
rebuffers with their duration and every source switch, manual or automatic, to
`POST /v1/telemetry/playback`; the server keeps daily counts by client kind, container, codec,
resolution class and bitrate class, and an admin reads them at `GET /v1/admin/telemetry/playback`.
Start failures are counted the same way, by reason. That is the evidence
[ADR-0014](decisions/ADR-0014-adaptive-streaming-rejected.md) asks for before constrained-bandwidth
or compatibility behaviour is judged insufficient. The client emitters are tracked in
[#222](https://github.com/justin13888/beam/issues/222), separately from the server half that landed
with [#143](https://github.com/justin13888/beam/issues/143).

Both endpoints authenticate via the session cookie like every other request; no tokens in URLs (see
`security.md`).

## Range requests and caching

The correctness bar is standard HTTP semantics, not media semantics:

- `Accept-Ranges: bytes` is always advertised.
- Single-range `Range: bytes=start-end` (including open-ended `bytes=N-` and suffix `bytes=-N`
  forms) returns `206 Partial Content` with `Content-Range`. A syntactically invalid or multi-range
  header is rejected with `400`; a range past end-of-file returns `416 Range Not Satisfiable`.
  Requests without a `Range` header get `200` with the full body.
- `Content-Type` comes from `files.mime_type` (with a container-derived fallback);
  `Content-Length` always reflects the bytes actually being sent.
- Responses carry `Cache-Control: public, max-age=3600` and a size-derived `ETag` as a coarse
  validator; a changed file is caught by the indexer's `mtime`/`hash_xxh3` change detection (see
  `data-model.md`).

## Subtitles

Subtitles are never burned in or composited server-side, and a subtitle stream inside a video is
never extracted: it is listed on its source with its stream index, codec, flags and `is_text`, for
the client to read from the stream it is already playing. The text subtitle files beside a video
-- indexed as `sidecar_subtitles` rows of it (issue #184, see `data-model.md`) -- are listed after
the embedded tracks, and served read-only
([ADR-0020](decisions/ADR-0020-text-subtitle-delivery.md)):

| Endpoint | Serves |
|---|---|
| `GET /v1/files/{fileId}/subtitles/{subtitleId}` | The file as stored, with its format's content type (`application/x-subrip`, `text/vtt`, `text/x-ass`, `text/x-ssa`), Range-capable through the same byte source and validator as file delivery. |
| `GET /v1/files/{fileId}/subtitles/{subtitleId}/webvtt` | A SubRip file converted to WebVTT, or a WebVTT file normalised to UTF-8 with LF line ends, as `text/vtt; charset=utf-8`, for a browser's `<track>`. Produced per request from the file on disk and never stored; its `ETag` derives from the file's modification time and length and the converter's revision. ASS, SSA and any file over 8 MiB are `404` `subtitle-rendition-unavailable`: fetch the track's `url` instead. |

A track offers the second exactly when it carries a `webvtt_url`. A subtitle id is only valid
beside the video whose sources listed it, and a subtitle of a video missing from disk is not
served. Both open the file never through a symbolic link and only as a regular file, and serve it
from that one handle; the rendition reads at most 8 MiB and converts in time linear in its length
(ADR-0020). Converting a subtitle is rewriting cue text with a pure function, not
transcoding media: see ADR-0020 for where that line is drawn.
