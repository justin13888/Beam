---
title: Caching
description: How caching works in Beam.
---

This page previously described a speculative distributed/Kubernetes-oriented caching design
(CDN layers, an on-the-fly-transcoding cache, a distributed indexer service). Beam's actual
architecture is a single modular-monolith server with no on-the-fly media generation to cache in
the first place -- see [ADR-0001: Modular Monolith](https://github.com/justin13888/beam/blob/master/docs/architecture/decisions/ADR-0001-modular-monolith.md)
and [ADR-0004: Never Transcode](https://github.com/justin13888/beam/blob/master/docs/architecture/decisions/ADR-0004-never-transcode.md).

What caching does exist today is ordinary HTTP caching (`Cache-Control`/`ETag`/`Range` on
streamed/downloaded files) plus standard client-side browser caching; there is no server-side
media cache, no CDN integration, and no Redis in the stack. For the current, maintained
architecture, see the engineering docs in the main repository:

- [`docs/architecture/overview.md`](https://github.com/justin13888/beam/blob/master/docs/architecture/overview.md)
- [`docs/architecture/streaming.md`](https://github.com/justin13888/beam/blob/master/docs/architecture/streaming.md)
