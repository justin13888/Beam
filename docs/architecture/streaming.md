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
the available versions with size, container, duration, and real probed per-stream codecs
(`H264`/`H265`/`AV1`, `Aac`/`Opus`, with an `UNKNOWN` fallback) plus each file's `stream_url` and
`download_url`. Selecting one is just a request against that file's own `/stream` endpoint. Sources
accept a movie id or an episode id; a show id is rejected with 400, since shows have no files of
their own. Episode sources landed in
[#102](https://github.com/justin13888/beam/pull/102), closing
[#68](https://github.com/justin13888/beam/issues/68). If an operator wants a low-bandwidth option,
they place a second, smaller rip in the library and let it get indexed — Beam does not create it.

That instruction holds for movies only. `IndexService` creates an episode row unconditionally, so a
second file for an episode collides with `idx_episodes_unique` and fails the scan, leaving
television with no alternate source at all — neither a smaller one nor a more compatible one. The
schema already permits it (`files.episode_id` is a plain foreign key); the fix is
[#142](https://github.com/justin13888/beam/issues/142), which
[ADR-0014](decisions/ADR-0014-adaptive-streaming-rejected.md) records as a condition of the
no-adaptive-streaming decision standing.

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

Subtitle tracks are not burned in or composited server-side; where present they are separate
`media_streams` rows for the client to handle.
