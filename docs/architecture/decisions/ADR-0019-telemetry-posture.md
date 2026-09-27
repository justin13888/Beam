# ADR-0019: Telemetry is opt-in, aggregate-only, and previewable

## Status

Accepted. Delivered with [#93](https://github.com/justin13888/beam/issues/93). Extended with
[Playback telemetry](#playback-telemetry) by [#143](https://github.com/justin13888/beam/issues/143)
(server half; the client emitters follow).

## Context

Beam had no usage telemetry of any kind: no analytics SDK, no crash reporting, no update check. The
operator FAQ promised exactly that. What it left the project without is any picture of what the
libraries it serves look like -- how many titles, which containers and codecs, how large -- which is
what decides where compatibility and performance work pays off (ADR-0004 and ADR-0014 both lean on
"which codecs actually occur in libraries").

A self-hosted media server is a privacy-sensitive place for telemetry to go wrong. A library's
titles and paths describe what its owner watches; a stable server identifier makes every report a
longitudinal record of one household. And #82's Prometheus `/metrics` endpoint is a different thing
entirely: operator-facing, pulled, and never leaving the operator's network.

## Decision

**Off by default, with no default collector.** `BEAM_TELEMETRY_URL` names an OTLP/HTTP metrics
endpoint the operator chose. Unset -- the default -- nothing is scheduled and nothing is ever sent.
There is no Beam-run collector compiled in, so opting in means naming a recipient, not ticking a box
that sends data somewhere the operator did not pick. An invalid URL fails startup rather than being
ignored: an operator who set it meant to opt in.

**Aggregate only, coarse, and nothing that identifies.** The report is counts and distributions:
libraries; live movies and shows, seasons and episodes; present files by content type and by
container; streams by type and codec; files by size range; the server's total indexed size as a
range; the Beam version. There is no title, path, library name, user, file hash, host name, instance
id or install id. No number is reported exactly: every count is an order of magnitude (`none`,
`from_1_to_9`, `from_10_to_99`, `from_100_to_999`, `from_1000_to_9999`, `at_least_10000`, read from
one table, `CountBucket`, in `beam-domain/src/utils/telemetry.rs`), and sizes are ranges too -- which
is the "coarse" shape #93 asked for, and still says which codecs and containers occur and at what
scale. Every free-form label is lowercased and stripped to `[a-z0-9_.,+-]` before it can leave the
process, so a path separator cannot ride out in a codec name. The timestamp is the UTC day, not the
instant of sending.

What this does **not** promise is that reports are unlinkable. A report contains no identifier and
its counts are bucketed, but a server whose shape is distinctive and stable -- an unusual codec, a
rare mix of ranges -- sends much the same report every week, and a collector may correlate those
reports, with each other or with the source IP. Operators are told so; the recipient is theirs to
choose.

**Preview before send.** `GET /v1/admin/telemetry/library` returns the report and the exact request
body that would be sent, byte for byte, with the collector's origin and the schedule. The payload is
a pure function of the report, so the preview is not a description of the request -- it is the
request. An admin can read it before deciding to opt in, and audit it after.

**OTLP/HTTP JSON, hand-written.** The body is an OTLP `ExportMetricsServiceRequest` of gauges in the
protobuf-JSON mapping, so any OpenTelemetry collector ingests it with no Beam-specific receiver.
Having no exact number to give, every point's value is `1` and its range is an attribute
(`count_bucket`, or `size_bucket` for the total size). A collector that keeps each request as it
arrives counts servers per range by summing; an OTLP backend may instead merge identical series (see
Consequences).
It is encoded by a hundred lines in `beam-server/src/services/telemetry/otlp.rs` rather than by the
OpenTelemetry SDK: one request a week of a dozen gauges does not need a periodic reader, exporter and
runtime, and a batching SDK cannot promise that what the preview shows is what goes on the wire.
This follows the posture Kynos takes in its own OpenTelemetry example -- the wire contract is the
operator's to own, in their crate.

**Weekly, and never soon after start.** The first report is sent an hour after start, then every 7
days; a failed delivery is retried after an hour, doubling to a day. The last delivery time is kept
in `BEAM_DATA_DIR/telemetry/library-report.json`, so a restart does not send early. The cadence is
fixed rather than configurable: a knob would add nothing an operator needs, and a shorter interval
only makes reports more linkable by timing.

**Playback stays local.** What users watch, when, and how far they got (FR-5xx progress, history) is
never part of any report. The report reads the library's shape, not its use. Playback telemetry
(below) is counted on the server and stays there: it is not part of this report either.

## Consequences

- NFR-503 records the privacy rule; FR-608 the admin preview.
- Every server's report carries the same resource attributes (`service.name`, `service.version`,
  `beam.report.schema_version`) and nothing that tells servers apart -- that is the point. An OTLP
  backend identifies a series by its resource and point attributes, so it may merge reports from
  different servers into one series, the latest overwriting the rest. A collector that wants one
  row per report should keep each export request as it arrives (a file or log exporter), or add a
  per-request attribute of its own on receipt; Beam will not add one.
- A new report field, or a change in a field's meaning, raises `schema_version` and is a change to
  this ADR's list above -- the list is the contract with operators, and the operator docs repeat it.
- Beam gains one more outbound HTTP client (`ReqwestTelemetrySink` in `beam-index`, beside the
  artwork fetcher), built with no cookie store and no redirects, so the report reaches the URL the
  operator named or nowhere.
- The collector URL is a secret in all but its origin: a query token or userinfo (which reqwest
  sends as a Basic `Authorization` header -- the supported way, with a query token, to authenticate
  to a collector) may sit in it. Only `scheme://host[:port]` is ever shown: the config's `Debug`
  output, the admin preview, and every delivery error -- the adapter strips the URL from each
  `reqwest::Error` before it is kept or logged.
- Everything above the network is hermetic: the shape is a repository trait with a shared contract
  (bound to the in-memory double and, under `pg-integration`, to the SQL), and delivery is a
  `TelemetrySink` trait with a recording double.

## Playback telemetry

[ADR-0014](ADR-0014-adaptive-streaming-rejected.md) settles adaptive streaming on the argument that
the failures viewers hit are capability failures, and hands the counter-argument --
constrained-bandwidth behaviour is insufficient -- to evidence nothing collected. #143 collects it.
What fails to start, what rebuffers and what switches source is far more sensitive than a library's
shape: a per-title failure record is a viewing record. So it takes this ADR's posture and tightens
it.

**Operator-local, with no phone-home at all.** Playback counts are kept in the server's database
and read by an admin at `GET /v1/admin/telemetry/playback`. They are never added to the library
report and never sent anywhere, whatever `BEAM_TELEMETRY_URL` says. A shared, cross-server picture
would be a separate decision with its own coarsening; none is made here.

**Off by default.** `BEAM_PLAYBACK_TELEMETRY_ENABLED` defaults to `false`. Unlike the library
report there is no URL to be the opt-in, so the switch is its own knob -- the issue's "default off"
is the stated reason for it. While off, `POST /v1/telemetry/playback` answers `409
playback-telemetry-disabled`: a 4xx a client's transport does not retry (the native core retries
5xx), and a distinct type a client reads as "stop reporting".

**Daily counters in three typed tables, not an event log.** `playback_start_counts`,
`playback_rebuffer_counts` and `playback_switch_counts` each hold one row per UTC day per
combination of coarse dimensions, incremented with `INSERT ... ON CONFLICT DO UPDATE`. A batch is
counted in one transaction, all or nothing: the native core retries a 5xx, so a batch that failed
part-way and still kept its first events would count them twice on the retry. A raw event
log, even one without a user column, is a timeline a determined reader can match against who was
watching; a daily counter is not. One generic `(kind, dimensions jsonb, count)` table was rejected:
it loses the `CHECK`s and the typed report, and every consumer would re-parse the dimensions.

**Dimensions the server derives, then forgets.** An event names a file only so the server can look
up its container, first video codec, chosen audio codec (the client's `audio_track_index`, else the
default track, else the first), resolution class (`sd` < 720 lines ≤ `hd` < 1080 ≤ `fhd` < 2160 ≤
`uhd`) and bitrate class (the file's average, else its video stream's). The file id goes no further
than that lookup, and codec and container names pass through the same `normalize_label` as the
library report. A file the server cannot resolve -- unknown, or missing from disk -- drops the event
rather than refusing the batch. The time recorded is the UTC day of arrival.

**Authenticated, then anonymous.** Reporting needs a session, which is what keeps the endpoint from
being an open write surface, and the handler passes no identity to the service. A batch holds at
most 50 events; there is no dedicated rate-limit class, since the same-origin rule, the session and
the batch bound already cap what one client can add, and a counter cannot be made to hold anything
but counts.

**One list per kind, and every reason with its stage.** A batch carries `starts`,
`start_failures`, `rebuffers` and `source_switches` as separate arrays, so each event's shape is
exact rather than a tagged union of optional fields. A failure carries one of five reasons
(`container`, `video_codec`, `audio_codec`, `network`, `other`) -- the vocabulary
`RejectionReason` in `beam-client-core` already has, coarsened -- and a stage (`preflight`, when the
client ruled the source out, or `playback`, when the player tried and failed). Successful starts
are counted too: a failure count means nothing without its denominator.

**Kept for a year, pruned daily, whether or not collection is on.**
`BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS` (default 365, 1 to 3650) bounds how long a day's counts
live; a loop on the injected `Clock` prunes at start and then daily. Turning collection off stops new
counts but does not purge old ones -- deleting an operator's data is a separate, explicit act, not a
side effect of a config change -- and the admin report stays readable either way.

Consequences for playback telemetry:

- NFR-503 carries the rule; FR-511 the ingest, FR-609 the admin report and retention.
- The report is a closed set of typed rows (`starts`, `start_failures`, `rebuffers` with a
  five-range duration histogram, `source_switches`) summed over at most 366 days, largest count
  first. None of it names a user, file or title.
- `validation-failed` enters the error vocabulary as a generic 422 with RFC 9457's per-pointer
  `errors` extension. Two Kynos 0.3.0 gaps bear on it, both tracked in #223 pending the upstream
  getkono/kynos issue, and neither is worked around locally (CLAUDE.md rule 3: recorded in a comment
  naming the tracking issue until a release carries the fix):
  - Kynos's `Json<T>` enforces serde only, not the `maxItems`/`minimum`/`maximum` its `Schema`
    derive publishes, so the service enforces every bound itself (`validate_batch`) until a Kynos
    release does. The gap is noted on `PlaybackTelemetryBatch`.
  - The derived problem responses describe `type` and `title` only, not `#[problem(extension)]`
    members, so the OpenAPI document's 422 carries no `errors` or `FieldError` and a generated
    client cannot type them; the wire and the reference docs carry them regardless. The gap is noted
    on `PlaybackTelemetryError::ValidationFailed`, and the document is regenerated, never patched.
- Where a file's container and codecs come from is one function (`ResolvedFile::of`) over today's
  stream model, so the unified stream model (#189) replaces it in one place.

## Alternatives considered

- **A Beam-operated default collector, opt-out.** Rejected: it makes every stock install report to
  the project unless its operator reads the documentation first, which is the posture the FAQ
  promised Beam does not take.
- **A custom JSON document.** Rejected: it needs a Beam-specific receiver, where OTLP lands in any
  collector an operator may already run.
- **The OpenTelemetry SDK.** Rejected as above: machinery for a stream Beam does not have, and it
  breaks the byte-for-byte preview.
- **Exact counts.** Rejected: an exact episode count held steady week to week makes one server's
  reports line up trivially, and #93 asked for a coarse shape. The orders of magnitude keep what
  the report is for -- which formats occur, at what scale.
- **An install identifier, for de-duplication.** Rejected: it turns a set of anonymous snapshots into
  a per-server history. Duplicates across a restart are prevented by the recorded last-send time
  instead.
- **An on/off flag separate from the URL.** Rejected: the URL is the opt-in. A second switch would
  only add a state -- enabled with nowhere to send -- that means nothing.
