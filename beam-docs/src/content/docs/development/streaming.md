---
title: Streaming
description: How streaming works in Beam.
---

Beam deliberately **never transcodes or remuxes on the fly**. It serves pre-existing media files
directly over range-request-capable HTTP (direct-play streaming and download), and where a title
has multiple pre-encoded versions, the client picks among them for constrained-bandwidth playback
instead of the server generating a new one on demand.

This page previously described an HLS/DASH/fragmented-MP4-remuxing design; that machinery has
since been removed from the codebase in favor of the direct-play model above. The canonical,
maintained source for the current streaming architecture is the engineering docs in the main
repository, not this site:

- [`docs/architecture/streaming.md`](https://github.com/justin13888/beam/blob/master/docs/architecture/streaming.md) --
  delivery scenarios, cache-friendliness, and the never-transcode rationale.
- [ADR-0004: Never Transcode](https://github.com/justin13888/beam/blob/master/docs/architecture/decisions/ADR-0004-never-transcode.md) --
  the decision record.
- [`beam-server` component doc](https://github.com/justin13888/beam/blob/master/docs/components/server.md) --
  how streaming/download endpoints fit into the server.
