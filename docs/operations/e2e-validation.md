# End-to-End Validation Runbook

This is the primary end-to-end confidence mechanism for this push. There is no automated
browser-driven test suite (no Playwright or equivalent) in this push — that is a deliberate scope
decision, not an oversight. Some integrations cannot be exercised hermetically by design (the real
OIDC ↔ Dex round-trip, real TMDB/AniList network calls — see `docs/testing/strategy.md`), so this
manual runbook, plus a set of curl-based API smoke checks, is what actually validates that the system
works end-to-end for a human before a release. Automated browser-based e2e is explicitly flagged as
future work, tracked separately from this push.

Prerequisites: complete `docs/operations/dev-setup.md` through step 5 (toolchains, hooks, `.env`,
codegen) before starting this runbook.

## 1. Bring up dependencies

```sh
podman compose -f compose.dependencies.yaml up
```

Brings up Postgres and Dex (the bundled dev OIDC provider — see
[ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)). Dex is preloaded with static
test users for local login: `admin@beam.localhost` and `user@beam.localhost`, both with password
`password`. Never use these credentials, or Dex itself, outside local development.

## 2. Run migrations

Apply pending `beam-migration` migrations against the compose Postgres instance. See
`beam-migration`'s own docs for the exact invocation; this push's schema additions (`sessions`,
`metadata_enrichment`, `anilist_id`, playback-progress columns, and the `stream_cache` drop) are
tracked there — see `docs/architecture/decisions` and the data model doc for the reasoning, not
re-derived here.

## 3. Generate fixture media

You do not need real copyrighted content — a couple of short synthetic clips generated with the
host's `ffmpeg` CLI (not the vendored Rust build) are enough to exercise the indexing, classification,
and enrichment code paths. Generate, into a directory you'll register as a library root:

- **A movie**, filename including a year (e.g. `Fixture Movie (2020).mkv`), to exercise
  movie-vs-show classification and year extraction.
- **A 2-episode show**, filenames following a season/episode naming convention (e.g. `Fixture Show
  S01E01.mkv`, `Fixture Show S01E02.mkv`), to exercise show/season/episode classification.
- **An anime title** with a name recognizable to AniList, to exercise the keyless AniList enrichment
  path independent of any TMDB API key (FR-306).

A minimal synthetic clip is sufficient, e.g.:

```sh
ffmpeg -f lavfi -i testsrc=duration=5:size=640x360:rate=24 -f lavfi -i sine=duration=5 \
  -c:v libx264 -c:a aac -shortest "Fixture Movie (2020).mkv"
```

## 4. Start `beam-server`

Using the vendored-ffmpeg local build if your host lacks system FFmpeg dev libraries (see
`docs/operations/dev-setup.md` step 2):

```sh
cargo build-local && ./target/debug/beam-server
```

Confirm your `.env`/shell environment has `BEAM_OIDC_ISSUER` and related OIDC variables pointed at the
local Dex instance, and `DATABASE_URL` pointed at the compose Postgres instance (see
`docs/operations/configuration.md`).

## 5. Start `beam-web`

```sh
cd beam-web && bun dev
```

## 6. Browser flow

Work through the following in order, using a real browser against the running dev stack:

1. Visit the web app (default `http://localhost:5173`). Confirm you are redirected to Dex.
2. Log in as `admin@beam.localhost` / `password`.
3. Create a library pointing at the fixture directory from step 3.
4. Trigger a scan and watch scan progress update live via SSE (FR-208).
5. Wait for enrichment to complete, then confirm the browse view shows posters, genres, and ratings
   for the enriched titles (FR-301, FR-310). The anime title should enrich via AniList even with no
   TMDB key configured (FR-306/FR-307); if a TMDB key is configured, the movie/show titles should
   enrich via TMDB.
6. Open a media detail page and confirm playback: play, seek, pause (FR-501, FR-509).
7. Reload the page mid-playback (or after playing partway through and navigating away) and confirm a
   resume prompt or a populated continue-watching entry appears, reflecting the last reported playback
   position (FR-507, FR-508, FR-701, FR-510).
8. Use instant search (debounced type-ahead) and confirm results navigate to the correct detail page
   (FR-404, FR-405, FR-703).
9. Open the admin area's log viewer and confirm recent scan/enrichment/auth events are visible
   (FR-604, FR-605, NFR-403).
10. Log out, then log back in as `user@beam.localhost` / `password`. Confirm no admin UI is visible
    (FR-606), and confirm a direct API call to an admin-only endpoint (e.g. `POST
    /v1/admin/libraries` or `POST /v1/admin/libraries/{id}/scan`) returns `403` for this non-admin
    session (FR-607, NFR-105).

## 7. curl-based API smoke checks

Run these against the running `beam-server` to validate security-relevant HTTP behavior that the
browser flow doesn't directly surface:

```sh
# 1. Unauthenticated request to a protected endpoint returns 401
curl -i http://localhost:8000/v1/media

# 2. Range request against the stream endpoint returns 206 with correct headers
curl -i -H "Range: bytes=0-1023" \
  --cookie "beam_session=<session-cookie-from-browser>" \
  http://localhost:8000/v1/files/<file-id>/stream

# 3. Download endpoint sets Content-Disposition: attachment
curl -i --cookie "beam_session=<session-cookie-from-browser>" \
  http://localhost:8000/v1/files/<file-id>/download

# 4. Cross-origin POST from a disallowed Origin gets 403 (CSRF defense-in-depth, NFR-104)
curl -i -X POST -H "Origin: https://evil.example" \
  --cookie "beam_session=<session-cookie-from-browser>" \
  http://localhost:8000/v1/admin/libraries/<library-id>/scan
```

Expected results: (1) `401`; (2) `206 Partial Content` with `Content-Range` and `Accept-Ranges: bytes`
present; (3) `200` (or `206` if combined with a Range request) with a `Content-Disposition: attachment`
header; (4) `403`.

Exact route paths above are illustrative — confirm against the generated `openapi.json`
(`beam-web/openapi.json`, produced by `bun run codegen:openapi:full`) for the actual endpoint paths at
the time you run this.

## Scope note

This runbook is the primary end-to-end confidence mechanism for this push given the explicit decision
not to build an automated browser e2e harness (no Playwright) this push. Automated browser-based e2e
is flagged as clearly-scoped future work — a deliberate deferral, not an oversight — once the REST
API and web client surface stabilize enough to make such a suite worth the maintenance cost.
