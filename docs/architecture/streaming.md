# Streaming & Delivery

Status: target architecture. See [ADR-0004](decisions/ADR-0004-never-transcode.md) for the full
rationale behind the decision this document describes the mechanics of.

## The governing rule: no server-side transcoding, ever

`beam-server` never transcodes, remuxes, or otherwise re-encodes media at request time. It serves the
bytes of whichever file on disk the request resolves to — nothing else. There is no ffmpeg process,
no fragmented-MP4 remux, no HLS segment generation anywhere in the request path. `beam-server` does
not link `ffmpeg-next` at all in the target state. The only place ffmpeg logic exists in the workspace
is inside `beam-index`, and only for reading technical metadata (resolution, codec, frame rate,
duration, bitrate, etc.) at index time, populating `media_streams` — never invoked while serving a
playback request.

This is a philosophy — "push complexity into rich clients, not into server-side transcoding" — not a
temporary limitation. It trades the ability to serve an arbitrary bitrate/format to any client for:
predictable, low server CPU/memory usage; the complete elimination of an entire class of
transcoding-pipeline bugs (the deleted HLS generator had 14 TODOs and a literal `panic!()` and was
never actually reachable in a working state); and a server that scales with disk and network I/O
instead of CPU.

## The three delivery scenarios

Every media request resolves to exactly one of these. They are first-class domain concepts — the API
surface and the client UI are organized around them, not just incidental HTTP behavior.

### (a) Full download

- **Trigger:** user explicitly requests a file for offline use.
- **Endpoint:** `GET /v1/files/{fileId}/download`
- **Headers:** `Content-Disposition: attachment; filename="…"` (derived from the file's original
  name, not the internal `file_path`); `Content-Type` from `files.mime_type`; `Content-Length`;
  `Accept-Ranges: bytes` and honors `Range` requests so downloads are resumable; `ETag` derived from
  `hash_xxh3` for cache/resume validation.
- **Behavior:** streams the file's bytes as-is. No transformation.

### (b) Direct-play streaming (the default, common case)

- **Trigger:** user presses play under normal (adequate-bandwidth) conditions.
- **Endpoint:** `GET /v1/files/{fileId}/stream`
- **Headers:** `Content-Disposition: inline`; `Content-Type` from `files.mime_type` (falling back to
  a sniffed/container-derived value if unset); `Accept-Ranges: bytes`; `ETag` from `hash_xxh3`;
  `206 Partial Content` responses to `Range` requests, `200 OK` with the full body otherwise;
  `Cache-Control` tuned for a byte-range media response (typically `private, no-store` or a short
  `max-age`, since the underlying file can change out from under a stale cache entry if the operator
  edits the library — see `mtime`/`hash_xxh3` change detection in `data-model.md`).
- **Behavior:** the player (Vidstack, in `beam-web`) issues standard HTTP Range requests as it
  seeks/buffers; `beam-server` reads the requested byte range directly off disk and streams it back.
  The browser's native media decoding handles whatever container/codec combination the source file
  actually uses. No remuxing, no container repackaging, no subtitle burn-in — subtitle tracks, where
  present, are exposed as separate resources for the client to fetch and render itself (see
  `api.md` for the subtitle-track endpoint shape), not composited into the video stream server-side.

### (c) Source-quality selection (constrained bandwidth / high latency)

- **Trigger:** the user (or the client, based on observed network conditions) chooses a lower-quality
  existing version of the same title, via a source-quality picker.
- **Endpoint:** same as (b) — `GET /v1/files/{fileId}/stream` — but resolved against a *different*
  `files.id`.
- **How selection works:** a single logical title (`movie_entries` row, or `episode` row) can have
  more than one `files` row associated with it — e.g. a 1080p remux and a separately-indexed 480p
  re-encode of the same movie, each scanned and cataloged independently. The API exposes the set of
  available file versions for a title (see `api.md`), each annotated with its `quality` label,
  container, and technical stream info from `media_streams`. The client presents these as a picker;
  selecting one is just a request against that file's own `/stream` endpoint.
- **What this is not:** this is not the server generating a new bitrate on demand. The "lower
  quality" stream is itself a full direct-play (scenario (b)) against a smaller file that already
  exists on disk, indexed the same way as any other file. If an operator wants to offer a
  low-bandwidth option for a title, they place a second, smaller rip of it in the library and let it
  get indexed — Beam does not create that file for them.

## What was deleted, and why

- **The stubbed HLS generator.** Never reached a working state (14 TODOs, a `panic!()` on an
  unimplemented path). Deleted rather than completed, because HLS/adaptive-bitrate streaming requires
  exactly the server-side transcoding this architecture rejects.
- **The fragmented-MP4 remux-on-the-fly path**, including its filesystem cache directory. Remuxing
  still constitutes server-side media processing at request time, which this architecture avoids even
  when it stops short of full re-encoding.
- **The `ffmpeg` CLI shell-out** (`Command::new("ffmpeg")`, used for subtitle handling in the remux
  path). Removing the remux path removes its reason to exist; subtitles are now served as
  independent, client-rendered resources.
- **The `stream_cache` table.** Tracked cache entries for the remux path above; with no remuxing,
  there is nothing to cache. Dropped by migration — see `data-model.md`.
- **`TranscodeService`.** The service class wrapping the above. Deleted along with its ffmpeg
  dependency; `beam-server` (renamed from `beam-stream`) no longer depends on `ffmpeg-next` at all.

## Range requests and caching, precisely

Because scenario (b) and (c) are both plain byte-range file service, the correctness bar is standard
HTTP semantics, not media semantics:

- `Accept-Ranges: bytes` is always advertised.
- Single-range `Range: bytes=start-end` requests return `206 Partial Content` with `Content-Range`.
  Multi-range requests may be rejected with a single-range fallback (`200` with full body) rather than
  implementing multipart/byteranges — players in practice issue single-range requests.
- `ETag` is derived from `files.hash_xxh3`, which is exactly the value the indexer already computes
  for change detection — reusing it means the same content always produces the same ETag without a
  second hash computation, and a changed file (different hash after a rescan) naturally invalidates
  any client/proxy cache keyed on it.
- `If-Range` / `If-None-Match` are honored for conditional requests, again keyed on the same ETag.

## Reference

See [ADR-0004](decisions/ADR-0004-never-transcode.md) for the accepted trade-offs of this approach
(most notably: Beam cannot serve an arbitrary bitrate to a bandwidth-constrained client unless the
operator has indexed one, and there is no adaptive mid-playback quality switching within a single
file — switching quality means switching to a different `files` resource, which most players surface
as a discrete "change source" action rather than seamless ABR).
