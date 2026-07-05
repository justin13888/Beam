# Component: `beam-docs`

Status: describes ownership and the intended division of responsibility going forward. This is not
an architecture document for the site's build tooling beyond what a contributor needs to know to
place content correctly.

## Role

`beam-docs` is a user- and operator-facing documentation **site** — install guides, feature tours,
configuration references for people running or using Beam — built with Astro + Starlight and
deployed to Cloudflare Pages via `wrangler` (see `astro.config.mjs`'s `@astrojs/cloudflare` adapter
and `wrangler.jsonc`).

It is deliberately separate from the root `docs/` tree (this ratification's home:
`docs/architecture/` — overview, data model, ADRs — `docs/requirements/`, and `docs/components/`,
this file's own directory). That split is intentional and should be preserved:

- **Root `docs/`** is the engineering source of truth: architecture, requirements, component
  ownership docs (this document and its five siblings), and ADRs. Its audience is contributors and
  agents working in the repository. It is plain markdown, reviewed in PRs alongside code changes,
  and is expected to describe the target/actual state of the system precisely (including deltas —
  see how [server.md](server.md), [indexer.md](indexer.md), etc. call out what's being deleted or
  added).
- **`beam-docs`** is polished, public-facing documentation for people who are not reading the source
  code. It may eventually be generated from or link back to root `docs/` content, but it is not
  meant to be a duplicate of it, and it should not be treated as a substitute for keeping root
  `docs/` accurate.

## Current state

The Starlight scaffold placeholders have been cleaned up as part of the G3 config/docs pass:

- `src/content/docs/index.mdx` — replaced the default Starlight splash page with a real landing
  page pointing to root `docs/`.
- `src/content/docs/guides/example.md` — deleted (was the default Starlight placeholder guide,
  never customized); its sidebar entry was removed from `astro.config.mjs` along with it.
- `src/content/docs/development/caching.md` and `src/content/docs/development/streaming.md` — both
  previously described the **old**, now-removed design (HLS/DASH adaptive-bitrate streaming as "the
  primary protocol," a server-side transcode/remux cache, a distributed/Kubernetes-oriented
  architecture). Per [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md),
  `beam-server` never transcodes — direct-play byte-serving only, with quality selection pushed to
  the library (multiple indexed file versions) rather than produced on demand. Both pages have been
  replaced with short pointers back to root `docs/architecture/` rather than re-deriving the design
  here, consistent with this document's own guidance below.

There is no other real content in the site yet beyond these two pointer pages. Root
`docs/architecture/` remains the accurate, detailed source for streaming/caching design.

## Working in this crate

- Content lives under `src/content/docs/`, following Starlight's collection schema
  (`src/content.config.ts`). Sidebar structure is configured in `astro.config.mjs`'s `starlight()`
  integration options.
- Biome (`bun run check`/`lint`/`format` from this directory) is the lint/format tool, consistent
  with `beam-web`.
- `astro check` (`bun run typecheck`) validates content/type correctness; there is no separate test
  suite for this crate today.
- Deployment is `astro build && wrangler pages deploy` (the `deploy` script); `wrangler pages dev
  ./dist` (`preview`) serves a local production build.

When adding genuinely new user-facing documentation (not covered by the caching/streaming follow-up
above), place it under `src/content/docs/` in a topic-appropriate directory, and prefer linking back
to root `docs/architecture/` for any claim about internal design rather than re-deriving it here.
