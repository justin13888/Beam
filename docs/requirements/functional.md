# Beam — Functional Requirements

Requirements use RFC 2119 keywords (MUST, MUST NOT, SHOULD, SHOULD NOT, MAY) to indicate normative
strength. Each requirement is independently testable. See `product.md` for narrative context and
`non-functional.md` for cross-cutting quality attributes.

## FR-1xx — Authentication & Session

- **FR-101**: The server MUST support authentication exclusively via OpenID Connect (OIDC). No
  password-based login, registration, or "forgot password" flow exists
  ([ADR-0003](../architecture/decisions/ADR-0003-oidc-bff-auth.md)). The identity provider is
  always the operator's own; Beam ships no IdP for deployment
  ([ADR-0016](../architecture/decisions/ADR-0016-bring-your-own-idp.md)).
- **FR-102**: The server MUST implement the OIDC Authorization Code flow with PKCE, performed
  entirely server-side. The browser MUST NOT receive, store, or handle ID tokens, access tokens, or
  refresh tokens at any point. The same holds for a native client signing in by the device
  authorization grant (FR-111): no client ever receives an IdP token.
- **FR-103**: On successful OIDC authentication, the server MUST establish a session identified by an
  opaque session credential, `beam_session`. A browser receives it as a cookie, which MUST be
  `httpOnly` and MUST use `SameSite=Lax`; a native client signing in by FR-111 receives the same
  opaque value in the response body and presents it as that cookie. Either way it is one session
  row with the same idle and absolute expiry.
- **FR-104**: Session state MUST be persisted server-side in Postgres
  ([ADR-0005](../architecture/decisions/ADR-0005-sessions-in-postgres.md)).
- **FR-105**: On a user's first successful login, the server MUST just-in-time (JIT) provision a
  local user record keyed by the `(issuer, subject)` pair from the OIDC identity token.
- **FR-106**: On every login, the server MUST derive the user's admin role solely from the configured
  ID-token claim (`BEAM_OIDC_ADMIN_CLAIM`/`BEAM_OIDC_ADMIN_VALUE`) asserted by the IdP, and MUST set
  or clear the stored admin role accordingly (granting **and** revoking). With no admin claim
  configured, no user is admin. The server MUST NOT expose any other mechanism to grant admin.
- **FR-107**: The web client MUST initiate login by redirecting the browser to `/v1/auth/login`. The
  client MUST NOT embed or invoke any OIDC client-side library.
- **FR-108**: The web client MUST determine current-session identity and role by calling
  `/v1/me`, and MUST treat a non-2xx response as "not authenticated."
- **FR-109**: The server MUST provide a logout endpoint that invalidates the server-side session and
  clears the session cookie.
- **FR-110**: For local development, the identity provider MUST be satisfiable by Dex running via
  `compose.dependencies.yaml`, configured with static test users, requiring no external network
  dependency. Dex MUST be opt-in rather than part of the default stack — Beam is
  bring-your-own-IdP (FR-101), so a default `compose up` MUST start no identity provider. When
  enabled, the bundled Dex MUST be reachable at one issuer URL that is valid both from the browser
  and from inside the server container.
- **FR-111**: The server MUST let a client with no browser sign in by the OAuth 2.0 device
  authorization grant (RFC 8628) against the configured IdP, run server-side:
  `POST /v1/auth/device` returns a user code, a verification URI and an opaque device handle, and
  `POST /v1/auth/device/token` polls with the handle, reaching the IdP at most once per poll interval
  and minting an FR-103 session on approval through the same admin (FR-106), provisioning (FR-105)
  and disabled-account rules as the browser callback. The IdP's device code MUST NOT leave the
  server, and only a hash of the handle MUST be stored. When the IdP's discovery document names no
  `device_authorization_endpoint`, `POST /v1/auth/device` MUST answer `501` so a client can fall
  back to the browser flow. See [ADR-0017](../architecture/decisions/ADR-0017-device-authorization-grant.md).

## FR-2xx — Library & Indexing

- **FR-201**: The server MUST run as a single binary (`beam-server`) that performs library indexing
  in-process; no separate indexer process or gRPC service boundary is required at runtime
  ([ADR-0001](../architecture/decisions/ADR-0001-modular-monolith.md)).
- **FR-202**: The server MUST access configured media library root paths in a strictly read-only
  fashion. The indexer MUST NOT write, rename, move, or delete files under a library root.
- **FR-203**: The server MUST maintain a separate, writable data directory (`BEAM_DATA_DIR`)
  distinct from any library root, used for its own state (e.g., the enrichment metadata cache).
- **FR-204**: The server MUST classify indexed filesystem entries into movies and TV
  shows/seasons/episodes, persisting them as `movie_entries` and `episode`-family rows respectively.
  Classification MUST read the file's path relative to its library root, folders included:
  - a season folder is one whose name carries a season word and number anywhere (`Season 01`,
    `Series 2`, `Saison`, `Staffel`, `Temporada`, `Breaking Bad Season 1`, `Season 1 (2008)`), a
    lone `S01` with no episode after it (`S01`, a season pack's `Show.S02.1080p`), or `Specials`
    (season 0); a folder carrying a range of seasons (`S01-S05`, `S01-05`, `Season(s) 1-5`,
    `Seasons 1 to 5`, optionally after `Complete`) is a multi-season pack, not a season folder, and
    the range ends the title it names (`Breaking.Bad.S01-S05.1080p` names *Breaking Bad*); a
    folder that is only a range (`Season 1-10`) names no title and is a pack inside its parent,
    which names the show as a season folder's parent does;
  - a show in a season folder is named by its series folder (the season folder's parent), unless
    the season folder's own text before its season token names another title or there is no
    series folder, when that text names it; else by the filename; a series folder that is the
    filename's show followed only by box-set words (`the`, `complete`, `series`, `collection`),
    or is only box-set words (`The Complete Series`), names the filename's show;
  - a show outside a season folder is named by the file's parent folder, unless the filename names
    another title (compared after identity normalisation), when the filename names it; with
    neither, `Unknown Show`;
  - an episode is numbered by an `SxxEyy`, `S01.E01` or `1x02` marker (of several, the last one
    before the first release-noise token, merged into a range with the markers written back to back
    before it for the same season with rising episodes), by an air date (`2024-03-01`: season =
    year, episode = `MMDD`), or by an absolute number (`Show - 012`, `E12`) only where the layout is
    unambiguous (a season folder, or a folder naming the same title); the `<title> - <n>` form also
    needs a season folder or a number of two or more digits; a year-shaped number (1900-2099) is an
    episode only in a season folder of the show the title names; inside a season folder only,
    `Episode N` / `Ep N` and a three- or four-digit number whose leading digits are the folder's
    season (`501`) number it too; a marker contradicting its season folder wins;
  - an episode's title is the text after its marker, else `Episode N`; a multi-episode file
    (`S01E01E02`, `S01E01-E03`, `S01E01.S01E02`) MUST attach to its first episode and record the
    last;
  - a movie is identified by title and year, taking its parent folder's year when the filename
    names the same title without one, and its parent folder's title when the filename's is empty or
    only release noise (never the library root's); each edition (a `{edition-...}` tag, or edition
    words such as `Director's Cut` or `Extended` after the title or its parenthesised year) of it in
    a library MUST be one `movie_entries` row however many copies exist;
  - one part of a movie split across files -- a trailing `cd`, `disc`, `disk`, `part` or `pt` and a
    number, set off by a space, dot, dash or underscore (`Movie (2019) - CD1`, `- Part 2`, `.pt1`,
    `disc1`) with nothing after it but release noise -- MUST key the movie its name spells without
    the token and record the number as its part; `part` and `pt` MUST count only after a release
    year in the name itself, never on the strength of the folder, since a title can end in them
    (`Harry Potter and the Deathly Hallows Part 1 (2010)`, `The Hunger Games Mockingjay Pt 1`) and
    one folder may hold two films of one year (`Che (2008)/Che Part 1.mkv`, `Che Part 2.mkv`) or
    a sequel beside its predecessor (`The Godfather (1972)/The Godfather Part 2.mkv`), and a token
    that opens the name or is followed by title words (`Part 2: The Sequel (2020)`) is the title's;
  - a file in a season folder with no episode number, a file in a range-only folder with no season
    and episode marker, a `<title> - <n>` name no folder names as a show, and a fractional
    `<title> - <n>.<d>` (`Show - 12.5`), MUST be indexed without a title
    (status `unknown`) and reported through the admin log, never guessed into a movie.

  An NFO beside the file and the file's container tags refine this (FR-219). Every row MUST record
  the version of these rules that classified it, and a scan MUST reclassify a row classified by an
  older version from its path, the NFOs beside it and the container tags its probe stored --
  keeping its id, hash and probe results -- so
  a change to the rules reaches files indexed before it.
- **FR-205**: The server MUST support multiple indexed file versions (distinct `files` rows) under a
  single logical movie or episode entry, to support the source-selection delivery scenario.
- **FR-206**: The server MUST detect and de-duplicate files that have already been indexed, based on
  filesystem identity and modification-time change detection, without re-processing unchanged files
  on subsequent scans.
- **FR-207**: The server MUST support triggering a library scan from an admin action in the web
  client; no manual out-of-band process invocation is required.
- **FR-208**: The server MUST emit scan progress events (started, per-item progress, completed,
  failed) over Server-Sent Events (SSE) for consumption by the web client. Each MUST name its scan
  job and carry the job's per-file counts; per-item progress MAY be throttled (it is sent at most
  once a second), and progress events MUST NOT displace other events from the recent-event log.
- **FR-209**: The server MUST support adding and removing library root paths via an admin-facing API,
  without requiring a server restart or manual configuration file edit.
- **FR-210**: A library scan MUST complete (or fail) independently of metadata enrichment; enrichment
  MUST NOT block or extend the scan's completion.
- **FR-211**: A file the indexer can no longer find on disk MUST be soft-deleted rather than
  removed: its row is kept, stamped `missing_since`, and excluded from browse, search, detail
  sources, streaming, continue-watching and history (including the history total); if the path
  reappears the row MUST be restored under the same id, so its playback progress survives a
  transient absence (an unmounted NAS, USB disk or bind mount). A row MUST be purged only by a scan
  that was not refused and found the file still missing after the configurable grace period
  (`BEAM_MISSING_FILE_GRACE_DAYS`, measured with the injected `Clock`). A walk error shields rather
  than vetoes: a row beneath a path the walk could not read -- a directory it could not list, or a
  listed entry it could not stat for any reason other than "not found" -- MUST be left untouched
  (neither marked nor purged), and a walk error with no path MUST leave every row untouched; rows
  elsewhere in the same scan are marked and purged as normal. Every walk error MUST be reported
  through the admin log. A watcher event whose path cannot be statted for any reason other than
  "not found", or a watcher removal while the library root is unavailable, MUST change nothing.
- **FR-212**: Library roots MUST be pairwise disjoint and disjoint from `BEAM_DATA_DIR`: registering
  a root that is, contains, or lies inside an existing library root MUST be rejected with
  `library-path-overlaps-library` (409), and one that overlaps the data directory with
  `library-path-overlaps-data-dir` (400), compared by whole components after canonicalization. At
  startup the server MUST refuse to start if `BEAM_DATA_DIR` overlaps a stored library root, and
  MUST log a warning, not refuse, for stored library roots that overlap each other. The
  indexer, the watcher and file delivery MUST NOT follow symbolic links beneath a library root; a
  link is not a library file, so a row whose path has become one is treated as missing (FR-211).
  The walk never follows a link. Every read of a library file that Beam parses, serves or records
  MUST resolve it relative to its library root with no symbolic link followed at any component
  beneath the root: neither the file nor any folder between it and the root. A file whose contents
  are read -- an NFO read by the indexer, a video the indexer hashes and probes, a video or a
  subtitle file delivered -- MUST be opened so; a file whose stat alone is recorded -- the stat of
  each entry the walk lists (a video's size, modification time and identity, a subtitle's size and
  modification time, an NFO's change stamp) and every later stat the indexer or the watcher compares
  with a row -- MUST be stat'ed so, from its folder so resolved, without following a link at the
  file itself. The root itself is opened as
  configured, so a root that is itself a link is followed. Delivery serves only a regular file so opened, read from that one handle,
  answering a link at any of those components, or a FIFO or device in the file's place, as
  `source-file-missing`. The indexer hashes and probes a video from one such handle, so nothing is
  ever recorded from a file that a link swapped in at the file or at any folder above it leads to.
  A stat or open that meets such a link fails, and the path is treated as missing, as a link the
  walk saw is. A folder, or a file, that the indexer's current pass already holds open is read as
  it was when opened, so for the rest of that pass its files may still read as present; the next
  pass meets the link, refuses it and marks the path missing.
- **FR-213**: A library whose root is on a network filesystem, or whose native watch hit the OS
  watch limit, MUST be polled every `BEAM_WATCH_POLL_INTERVAL_SECS` instead of relying on native
  events, with no configuration switch, and MUST be scanned once when it starts being polled so
  changes made before polling began are not missed (for a library polled from startup, the startup
  scan is that scan: the background indexer MUST register every watch before the startup scan
  starts, and MUST schedule no single-library scan alongside it); a library MUST be watched as soon
  as it is created -- registered in the background, so creating it does not wait on a walk of its
  tree -- and stop being watched as soon as it is deleted, with the periodic maintenance cycle as
  the backstop for a registration that failed or landed after the delete; the admin status MUST report each library's watch mode and whether the watch limit has
  been reached. Every scan of a library is serialised (FR-218).
- **FR-214**: The indexer MUST match a file to an existing movie or show by an identity key derived
  from the filename parse -- the normalised title and year, and for a show its series folder --
  stored apart from the display title and never changed by enrichment, so a title enrichment renamed
  still receives its later files. Finding or creating a title by its key MUST be atomic: files of
  one new title indexed concurrently MUST resolve to one title. A key MUST record the version of
  the classification rules (FR-204) that derived it; before a scan reclassifies any file, a key an
  older version derived MUST be re-derived from the title's files, in place (the title keeping its
  id and enrichment), and two titles the current rules key alike MUST be merged into one, keeping
  the one a provider matched, else the older, and carrying to it the other's administrator's pin
  and locked fields (FR-313) -- a survivor an administrator pinned keeping its own pin. Every path that reclassifies -- the scan of every
  library, the administrator's scan of one, a watcher event -- MUST first run the identity-key
  backfill and re-derivation if they have not succeeded in the process, and while they have not
  (they failed, and are retried by the next such path) MUST NOT reclassify a file an older version
  classified, still indexing new and changed files; a failed pass MUST be reported through the
  admin log. The passes MUST run while no scan or watcher reconcile classifies a file, so a key they
  move is never taken by a file indexed in between.
- **FR-215**: A movie or show with no present file MUST be excluded from browse and search as soon as
  its last file is soft-deleted (FR-211), while remaining resolvable by id; it MUST be deleted,
  with its enrichment state, only by a scan whose walk read the whole library and only once no file
  row -- present or soft-deleted -- is left for it.
- **FR-216**: The indexer MUST index only video files. It MUST NOT index, hash or probe hidden files
  or anything under a hidden folder; NAS and operating-system housekeeping folders (`@eaDir`,
  `#recycle`, `$RECYCLE.BIN`, `System Volume Information`, `lost+found`); any file of a DVD or
  Blu-ray disc structure its main title does not play (FR-222), and everything in an `AUDIO_TS` or
  `CERTIFICATE` folder, at any depth; extras folders below the top level of a library (`Extras`, `Featurettes`, `Behind The
  Scenes`, `Deleted Scenes`, `Interviews`, `Sample(s)`, `Bonus`), and the extras folder names that
  can also name a category (`Scenes`, `Shorts`, `Trailers`, `Other`) only inside a title's folder --
  one naming a title and year, a season folder, or any folder below the top level of a library;
  files named as extras (`-trailer`, `-sample`, `.sample`, `_sample`, `-featurette`,
  `-behindthescenes`, `-deleted`, `-interview`, or exactly `sample`/`trailer`); or paths matching an
  administrator's `BEAM_SCAN_IGNORE` glob patterns, which live in server configuration, never in
  files written into a library (FR-202). An invalid pattern MUST fail startup. The full scan and the
  watcher MUST decide identically; a row for a path that is no longer indexed MUST be soft-deleted
  (FR-211).
- **FR-217**: A library root whose walk found video files -- including ones FR-216 excludes by name
  or by an ignore pattern matching the file -- MUST NOT be refused as unmounted by the empty-root
  guard; only a root with no such video files, under a library with indexed video files, is refused.
  The walk never descends into a folder FR-216 excludes (hidden, housekeeping, `AUDIO_TS` or
  `CERTIFICATE`, extras, or matched by an ignore pattern), so video files under one are not counted:
  a root holding only those is refused. A disc structure's stream files (FR-222) are video files,
  whether or not its main title plays them, so a library holding only disc folders is not refused. This errs toward refusing -- the safe side, since a refused scan changes no
  rows.
- **FR-218**: A library scan MUST be a job: requesting one MUST answer once the job is registered
  (`202` with the job) rather than when the scan finishes, and the latest job of each library MUST
  be readable. Every scan of a library -- administrator's, startup, periodic, newly polled -- and
  every watcher reconcile of it MUST be serialised: a request while a scan is queued or running MUST
  be refused with `library-scan-in-progress` (409), the periodic rescan MUST skip such a library,
  and a watcher event MUST be deferred and retried rather than wait. A file MUST NOT be hashed until
  it has gone `BEAM_SCAN_SETTLE_SECS` without a write (measured with the injected `Clock`), and a
  file whose probe failed MUST be probed again on each visit and classified when a probe succeeds;
  when a file's content changes and its probe fails, the old content's probe results and streams
  MUST be cleared rather than kept; when its size, modification time or identity (FR-221: inode and
  change time) moved but its content did not, the new size, modification time and identity MUST be
  recorded so it is not hashed again on the next visit. At most one `files` row MAY exist per
  path. Deleting a library MUST first stop anything from starting on it -- a scan of any trigger,
  or a watcher reconcile -- then fail a queued scan of it as cancelled at once and wait (bounded)
  for a running one to stop; if the delete itself then fails, the library MUST be scanned and
  reconciled again as before.
- **FR-219**: Classification (FR-204) MUST also read the Kodi-style NFO describing a file --
  `<stem>.nfo` beside it, else `movie.nfo` in its folder, and for an episode `tvshow.nfo` in its
  folder or, when that is a season folder, the series folder above; never one at the library root
  or in a category folder above a show's own folder -- and the file's container tags, in the
  priority NFO, then path, then tags. The container tags a probe read MUST be stored with the
  file, replaced by each successful probe and cleared when changed content fails its probe, so a
  reclassification reads them without probing the file again; a text tag MUST be kept to at most
  512 bytes, cut on a character boundary. An NFO's root (`<movie>`, or `<episodedetails>` with a
  season and episode) decides whether the file is a movie or an episode; container tags (`show`,
  `season_number`, `episode_sort`, `title`, `date`/`year`) only fill what the path leaves open. An
  NFO or a tag MUST NOT change the identity key a title is matched by (FR-214), which stays the
  path's; it supplies the display title and year a new title is created with. A provider id in a
  movie or show NFO (`<uniqueid>`, a legacy id element, or a provider URL; TMDB, then AniList, then
  IMDb, then TheTVDB) MUST pin the title: a file whose NFO names a pin MUST join the title pinned
  to it, or enriched with that id, before its key is consulted; one id pins at most one title; a
  second NFO naming another id for a pinned title MUST be reported through the admin log and not
  applied, then or at any later scan. An NFO added or edited after indexing MUST re-pin, at the
  next scan or watcher event, exactly the titles of the files it is *the* NFO of -- located as
  above, so never through a root NFO, one further above a file, or a `movie.nfo` beside a file
  with its own `<stem>.nfo` -- but never an administrator's pin (FR-312). Whether an NFO is
  re-applied MUST turn on its content alone: the size and content hash last applied are recorded
  per NFO (`applied_nfos`), so an NFO whose modification time is old (`cp -p`), skewed by another
  host's clock, or older than a scan that died is still applied, and one whose content did not
  change is never applied again. An NFO whose pin is refused because another title already holds
  that id MUST NOT be recorded as applied, so a later scan tries it again -- unless the title
  holding it got it from that same NFO (one describing several titles: a `movie.nfo` beside two
  movies, a `tvshow.nfo` over episodes of two shows), which no retry can change: such an NFO MUST
  be recorded as applied, pin one title -- the one with the lowest id, so the same one at every
  scan -- and be reported once through the admin log. Deleting
  an NFO forgets its record and leaves the pin it set; an NFO created at that path again is one
  added after indexing and re-pins, so a kept, conflicting NFO deleted and recreated replaces the
  title's pin. That holds only when Beam saw the NFO gone, and the same whether the NFO alone or a
  folder holding it went -- a watcher removal event for either, or a scan while it was missing; one
  put back unseen with the same bytes is an unchanged NFO. A video relinked to its row (FR-221) is
  not classified again, so the NFOs classification would read for it at its new path MUST take
  their applied state by one rule, judged for every video one scan or watcher event relinks
  together, against the records as they stood before, so that neither the order the videos are
  met in nor how their folders' names sort changes the outcome: an NFO whose content its path's
  record does not hold (or that has no record) has *moved* when its content is what the record of
  an NFO path one of those videos had holds, and the file there no longer holds it -- gone, or
  holding other content. An NFO several of those videos locate (a folder's `movie.nfo`, a show's
  `tvshow.nfo`) MUST be judged once, over all of them. A moved NFO MUST be recorded as applied and
  change no pin -- a kept, conflicting NFO stays kept -- whether it moved to a free path or, in a
  swap or rotation of files or folders, to a path whose own NFO moved on in turn. An NFO whose
  content no such record holds is an edited or a new one: at a path already recorded, or where an
  NFO any video locating it had at its old path is gone from disk and recorded, it is edited and
  applied as edited; one whose path's record already holds its content is left alone, so a video
  moved beside an NFO already recorded does not take it; any other -- never applied to the title,
  or forgotten by a removal seen first -- MUST be applied, to every video locating it, as a new
  file's NFO is, keeping a title's other pin, and recorded. An NFO forgotten by a removal and put
  back at the same path is therefore one added after indexing and re-pins its title, while one
  that comes back with its video at another path keeps the title's pin. A watcher event sees a
  move one name at a time, so it MUST leave a changed NFO as it is -- neither applied nor
  recorded -- for the next scan to judge as above when the NFO may be one half of a move: a video
  it may describe was left to the scan by the same event (FR-221) or is no longer the file its
  row records, or its content is what another NFO path's record holds and the file there no
  longer holds it.
  A walk that could not read where an NFO lives, or a removal reported while the library root is
  gone, MUST NOT forget its record. A watcher event MUST read only the files beneath the NFO's
  folder. Every NFO MUST be read with a read-only open of a regular file (never through a
  symbolic link at any component beneath the library root, FR-212 -- and on Unix with
  `O_NONBLOCK`, so a FIFO cannot stall the read) and at most 1 MiB of it; an NFO larger than
  that, not
  UTF-8, declaring a document type, or over 10 000 XML nodes MUST be ignored and the file
  classified by its path. Reading these MUST NOT write anything under a library root (FR-202).
- **FR-220**: A text subtitle file (`.srt`, `.vtt`, `.ass`, `.ssa`) beside an indexed video, named
  after the video's filename stem (the longest stem when several match) or in a `Subs`/`Subtitles`
  folder beside it, MUST be recorded as a subtitle of that video in `sidecar_subtitles`, with the
  ISO 639-2/B language, forced, SDH and default flags, and title its name's tokens give. A scan MUST
  record new and changed subtitles, writing only what changed, and delete the rows of subtitles no
  longer found or no longer owned by an indexed video -- except beneath a path the walk could not
  read (FR-211); the watcher MUST do the same for a single subtitle, and a video it indexes MUST
  pick up the subtitles already beside it. A video relinked to its row (FR-221) MUST own the
  subtitles beside its new path, and no longer those beside its old one: the scan judges subtitles
  and NFOs after its relinks, and the watcher reconciles a relinked video's subtitle rows, and the
  subtitles and NFOs beneath a directory it reconciles. An NFO's record stays keyed by the NFO's own
  path (FR-219). Image-based subtitles are not indexed. Serving them is FR-512.
- **FR-221**: A file's `files` row MUST follow the file's content within its library. A path whose
  content hash (non-zero) and size match a row of the same library whose own content has left its
  path -- the path is gone, the row is marked missing (FR-211) and its path not walked, or the path
  now holds different content -- MUST be relinked to that row, whether the path is new or already
  has a row of its own. This covers a move or a rename, two files swapping names, a rotation among
  several, and a rename onto the path of a row already missing. A relinked row keeps its id and so
  its playback progress, its movie or episode, and its streams, and MUST NOT be probed or classified
  again. A path whose content matches no such row keeps the existing behaviour: an indexed path's
  row has changed content, a new path gets a new row, and a file whose original is still at its path
  is a copy with a row of its own. A full scan MUST decide every path it walked at once,
  deterministically -- every path gets at most one row and every row at most one path -- and MUST
  apply the relinks atomically, one row per path holding when they are done. A scan MUST NOT take
  a path's size and modification time alone as proof that it still holds its row's content: on
  Linux and macOS it MUST also record each file's inode and change time, and hash a path whose
  inode or change time is not its row's, so a swap or a rotation of files of one size and one
  modification time (written within one timestamp tick, or copied by a tool that keeps mtimes) is
  relinked like any other. On a library whose root the watcher classifies as a network or FUSE
  filesystem (FR-213's network filesystems), whose inode numbers may change between scans, the
  inode MUST NOT be compared -- only the change time -- so an unchanged library is not hashed again
  on every scan. A filesystem that reports no change time of its own -- sshfs and rclone mount
  report a file's modification time as its change time -- gives that comparison nothing to add, so
  on such a library a swap of files of one size and one modification time is not detected; NFS and
  SMB, which report a real change time, are unaffected. A row with no inode and change time recorded
  MUST be given its file's by the next visit that finds it unchanged, without hashing it. Among
  several rows for one path, and several paths for one row, the pairing with the same file name
  wins, then the same directory, then the row played most recently by anyone (a row never played last), then the lowest
  row id, then the lowest path. One pairing is declined, a replace-by-rename: when the row a path
  has -- present or missing -- would otherwise be displaced (its own content is at no path the scan
  hashed, or that path went to a better pairing), has been played by anyone, and the row whose
  content is now at that path never has, the path MUST keep its own row, its new content a change to it as for any other
  path, and the other row MUST NOT take the path; that row is paired elsewhere if the scan finds its
  content elsewhere, and otherwise is marked missing like any gone file. Every other case follows
  the order above. A row whose path a relink takes and whose content is at no path the
  scan hashed MUST NOT be deleted: it is soft-deleted beside its old path, where it can still be
  relinked by content and is purged after the grace period like any missing row. A watcher event
  MUST relink a new path to a row whose file is gone, whichever of a move's two events is reconciled
  first; one whose content matches a row of the library that may have moved -- marked missing, or
  its path not holding what the row recorded -- and that it cannot relink without displacing a row
  MUST be left untouched for the next scan rather than guessed at. Every relink MUST be reported
  through the admin log with both paths, and a scan's progress MUST count relinked files. A watcher
  event naming a directory MUST reconcile what is beneath it: a directory renamed or moved in is
  walked and each file reconciled, and the rows beneath a removed directory whose files are gone are
  soft-deleted -- unless the library root holds no video file, which reads as an unmounted volume
  as it does for the scan (FR-211), and leaves the directory to the next scan; the watcher never
  purges.
- **FR-222**: A DVD or Blu-ray disc structure copied whole -- a `VIDEO_TS` or `BDMV` folder, in any
  case, at any depth the policy of FR-216 does not exclude -- MUST be indexed as one source of the
  movie the folder enclosing it names, read as a filename is (FR-204) and completed from the folder
  above it (`Heat (1995)/VIDEO_TS` is *Heat* (1995); `Heat (1995)/DVD9/VIDEO_TS` too). No file
  inside a disc is judged by its own name. A disc at the library root, or in a season folder, names
  no title: its files MUST be indexed without one and the administrator told. The source MUST play
  the disc's main title, read from the disc: for a DVD, the title set whose longest program chain
  lasts longest by its `VTS_nn_0.IFO` -- or, when any title set's IFO cannot be parsed, the title
  set with the most bytes -- whose `VTS_nn_1.VOB`, `VTS_nn_2.VOB`, ... up to the first missing part
  play in turn; for a Blu-ray, the `BDMV/PLAYLIST/*.mpls` whose play items last longest among
  those whose every clip is in `BDMV/STREAM`, its clips played in the playlist's order, each once --
  or, with no such playlist, the largest clip alone. A main title of several files MUST be one
  source of ordered parts (FR-505); a file's part is its place in the main title and MUST be kept
  when a rekey re-derives its title from its path (FR-214). Reading a disc MUST follow no link
  (FR-212) and MUST NOT write anything; a disc that cannot be read whole MUST NOT change its rows,
  as beneath a folder the walk could not read (FR-211). A watcher event at or inside a disc MUST
  reconcile the disc whole. A disc's files are served by direct play as they are -- MPEG program
  stream VOBs and BDAV MPEG transport stream clips -- and never remuxed or transcoded (ADR-0004);
  the source MUST name its disc structure (`disc_structure`), so a client that cannot play them
  lists the source as not directly playable, with its reason (ADR-0014).

## FR-3xx — Metadata Enrichment

- **FR-301**: The server MUST enrich indexed movies and shows with metadata (poster URL, backdrop
  URL, description, ratings, genres, release date) via the `cameo` crate, which unifies TMDB and
  AniList sources ([ADR-0006](../architecture/decisions/ADR-0006-cameo-enrichment.md)).
- **FR-302**: Metadata enrichment MUST run as a background pipeline that begins after a title has been
  indexed and classified, and MUST NOT block the indexing/classification scan itself.
- **FR-303**: The server MUST persist enrichment status per title (e.g., pending, enriched, failed)
  and MUST make that status queryable by the admin area: a list of titles filterable by status and
  kind, each with its last error, and each title's own status.
- **FR-304**: On enrichment failure, the server MUST retry with backoff rather than permanently
  marking the title as failed after a single attempt.
- **FR-305**: The server MUST enrich TV show titles at the season and episode level (season/episode
  titles, descriptions, air dates), not only at the top-level show.
- **FR-306**: Enrichment MUST proceed for AniList-sourced titles without requiring any API key. The
  absence of a configured `BEAM_TMDB_API_TOKEN` MUST NOT prevent AniList-sourced titles from being
  enriched.
- **FR-307**: If no `BEAM_TMDB_API_TOKEN` is configured, the server MUST leave TMDB-eligible titles
  un-enriched (rather than failing indexing or scan completion) and MUST surface this condition in
  admin-visible enrichment status: the admin status reports each provider as configured, not
  configured, or configured but unavailable.
- **FR-308**: The server MUST expose an admin-triggerable "re-enrich" action, scoped to a single title,
  to one library's titles, or to all titles, that re-runs enrichment regardless of current status,
  keeping each title's match unless the administrator asks for a rematch.
  Operator-facing enrichment tuning knobs (batch size, minimum confidence, metadata language) are
  deferred — tracked in [#71](https://github.com/justin13888/beam/issues/71).
- **FR-309**: The server MUST emit enrichment progress/status-change events over SSE, in the same
  manner as scan progress (FR-208): one per title an enrichment pass attempts, naming the title and
  carrying its new status, match and error; like progress events, they MUST NOT displace other
  events from the recent-event log.
- **FR-310**: The server MUST serve every piece of artwork itself — movie and show posters and
  backdrops, season posters, episode thumbnails — from `/v1/artwork/{kind}/{id}/{variant}`, fetching
  each image from the provider once and caching it on disk. `poster_url`, `backdrop_url` and
  `thumbnail_url` MUST carry that path, never a provider URL, so no client contacts TMDB or AniList
  ([ADR-0015](../architecture/decisions/ADR-0015-artwork-served-by-beam.md), NFR-501). A title with
  no artwork, an unknown id, and a variant that does not apply to that kind of title MUST all be
  `404`; clients render a placeholder.
- **FR-311**: An artwork URL MUST remain stable when enrichment refreshes a title's art, so that a
  client which stored one — a download record, rendered offline — still resolves. Freshness is
  carried by the `ETag` instead, which is derived from the provider URL and therefore changes
  exactly when the artwork does.
- **FR-312**: A title an NFO pins (FR-219) MUST be enriched in this order: a match an administrator
  set first; else, when a configured provider resolves the pin's id (TMDB or AniList), a fetch by
  that id at full confidence, with no search; else a search as usual, whose match MUST be left
  unmatched, with the reason recorded, when it carries a different id of the pinned provider.
  Pinning a title, or re-pinning it, MUST queue it for enrichment with its stored match cleared; a
  rematch MUST clear the match and keep the pin. A pin MUST record who set it, an NFO or an
  administrator: an administrator's manual match
  ([#185](https://github.com/justin13888/beam/issues/185)) sets an administrator's pin, which takes
  precedence over any NFO's and which no NFO -- re-read, edited, or named by a new file -- replaces,
  so an administrator never has to edit a file in a library to correct Beam.
- **FR-313**: An administrator MUST be able to correct a title's enrichment without touching the
  library ([#185](https://github.com/justin13888/beam/issues/185)): search the configured providers
  for candidates; fix the match to a chosen id, which sets an administrator's pin (FR-312) and
  queues the title with its old match cleared -- and clear that pin again, returning the title to
  its NFO's pin or to a search (clearing a title no administrator pinned changes nothing); and lock individual fields (title, original title, description,
  year, release date, runtime, poster, backdrop, rating, genres), which enrichment MUST then leave
  as they are. A fetch by an id the provider answers it has no title for MUST be retried with
  backoff (FR-304), naming the id, for a title a pin holds -- an NFO's or an administrator's --
  and leave it unmatched rather than failed once the retries are spent; a title with no pin a
  configured provider resolves, whose stored match the provider no longer has, MUST be searched
  for again by its name instead.

## FR-4xx — Browse, Search & Detail

- **FR-401**: The web client MUST provide a library browsing view listing movies and shows with
  poster art, title, and, where available, genres and rating. The browse endpoint returns each
  title's genres, rating and external identifiers, and each show's season and episode counts, so
  a tile needs no further request; it accepts a `genre` filter by name or slug
  ([#187](https://github.com/justin13888/beam/issues/187)).
- **FR-402**: The server MUST expose a movie detail endpoint returning enriched metadata and the set
  of available file versions (for source selection, per FR-205).
- **FR-403**: The server MUST expose a show detail endpoint supporting season and episode
  enumeration, including enriched per-episode metadata where available.
- **FR-404**: The server MUST provide a search endpoint that performs matching server-side using
  Postgres `pg_trgm` similarity, and MUST NOT implement search by loading the full title set into
  application memory and filtering in Rust.
- **FR-405**: The web client MUST provide debounced type-ahead search that queries the search
  endpoint as the user types and renders results without a full page navigation.
- **FR-406**: Search results MUST include enough information (title, poster URL, media type, id) for
  the client to render a result list and navigate directly to the corresponding detail page.
- **FR-407**: The client-facing API MUST expose only domain identifiers (e.g., title id, file id) in
  browse/search/detail responses, and MUST NOT expose raw filesystem paths (see NFR-601).

## FR-5xx — Playback, Streaming & Download

- **FR-501**: The server MUST serve file bytes for direct-play via HTTP Range requests, without
  server-side transcoding or remuxing of any kind
  ([ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md)).
- **FR-502**: The server MUST serve file bytes for full download as an attachment response, and MUST
  support Range requests to allow download resumption.
- **FR-503**: The server MUST NOT generate or serve HLS/DASH manifests or segments. This is
  settled, not deferred: both formats are defined over segments, so producing them for a library's
  existing containers means repackaging bytes — request-time remuxing or an index-time derived
  artifact, each of which FR-501 and
  [ADR-0004](../architecture/decisions/ADR-0004-never-transcode.md) exclude. Adaptive bitrate over
  independently indexed encodes is separately unachievable, since their keyframes do not align. See
  [ADR-0014](../architecture/decisions/ADR-0014-adaptive-streaming-rejected.md) for the full
  argument and for the client-side work that answers the compatibility and bandwidth problems
  instead.
- **FR-504**: Streaming and download endpoints MUST authenticate the request using the session
  cookie established per FR-103. The server MUST NOT accept a bearer or stream token supplied via URL
  query string.
- **FR-505**: For a title with multiple indexed file versions, the server MUST expose an endpoint
  (`/media/{id}/sources`) enumerating the available versions — container, size, edition, and each
  version's video, audio and subtitle tracks, every track tied to its file by its stream index and
  naming its real codec as FFmpeg does, with language, title and default/forced flags — so the
  client can present a source-quality picker and choose tracks. A value the file does not state
  MUST be absent, never a substituted default. A file holding a run of episodes MUST say so. The
  parts of a multi-part movie that share an edition and a folder MUST be one version listing every
  part's file in part order, each with its own stream URL, so a client can play them in sequence
  without the server joining them, when their numbers run 1..n with no gap or repeat (otherwise
  each file MUST be a version of its own); its size and duration are all the parts'. The
  endpoint accepts a movie id or an episode id; a show id is rejected, since shows have no files of
  their own.
- **FR-506**: Switching between file versions during the source-selection scenario MUST result in
  direct-play of the newly selected file; the server MUST NOT perform any transcoding or format
  conversion to service the switch.
- **FR-507**: The server MUST track and persist each user's playback position ("resume point") per
  title -- a movie or an episode -- as the client reports progress in any of the title's files, so
  that switching to another source of the title resumes from the same place; it MUST remember the
  file last played. A report reaching 95% of the duration MUST mark the title played, count a play
  and return the position to the start; a later report short of the end MUST NOT unmark it. The
  server MUST refuse (422) a negative or non-finite position, a non-positive duration, and a
  position past the end -- the reported duration, else the file's probed one -- by more than 2
  seconds or 1% of it, whichever is larger, taking a position within that as the end. For a
  multi-part movie a position is kept within the part reported, and the duration is that part's:
  only a report on its last part MAY mark the movie played, and a report on an earlier part MUST
  record its position without marking it.
- **FR-508**: The server MUST expose an endpoint returning the current user's continue-watching
  rows, most recently played first: one per movie with a resume position, and one per show at the
  episode to watch next -- the episode last touched if it has a position, else the first later
  episode in (season, episode) order, across seasons, that has a present file and is not played --
  omitting a show with no such episode and a title with no present file. A next episode the user
  already started resumes from its own position. Each row MUST carry what a client displays
  (title, artwork, season and episode numbers and title) and the file to play: the one last played
  while present, else the title's primary (FR-513). One request MUST examine at most the 1,000 most
  recently played candidate titles -- movies with a position and shows, each played since it was
  last removed -- so its cost is bounded however many shows the user has finished; a title past
  that ceiling is not listed, and the response's `page_info.has_next_page` MUST say whether
  candidates were left unexamined, because the rows filled first or the ceiling was reached.
- **FR-509**: The web client's player (Vidstack-based) MUST support seeking, keyboard shortcuts,
  visible buffering state, fullscreen, and Picture-in-Picture.
- **FR-510**: On resuming a previously started title, the web client MUST seek playback to the
  last-reported resume position (per FR-507) rather than starting from the beginning, subject to user
  override. The server MUST expose the current user's resume position and played state for a movie
  or an episode by its id, and let the user clear the position.
- **FR-511**: When the operator enables playback telemetry (`BEAM_PLAYBACK_TELEMETRY_ENABLED`), the
  server MUST accept authenticated batches of playback starts, start failures (with a reason --
  container, video codec, audio codec, network, other -- and a stage -- preflight or playback),
  mid-stream rebuffers with their duration, and source switches (manual or automatic), and MUST
  count each only as a daily aggregate under coarse dimensions derived server-side from the file it
  names (client kind, container, codecs, resolution class, bitrate class), discarding the file and
  the reporting user (NFR-503). A file it cannot resolve MUST be dropped, not refused. When
  telemetry is disabled the endpoint MUST refuse with a distinct 409 so clients stop reporting.
- **FR-512**: The subtitle files indexed beside a video (FR-220) MUST be listed among its source's
  subtitle tracks, and the server MUST serve each read-only: as stored, Range-capable, with its
  format's content type; and, for SubRip and WebVTT files up to 8 MiB, as WebVTT, SubRip converted
  and WebVTT normalised to UTF-8. Rewriting a text subtitle is not transcoding (FR-501,
  [ADR-0020](../architecture/decisions/ADR-0020-text-subtitle-delivery.md)). A subtitle stream
  inside the video MUST NOT be extracted; it is listed with its stream index and no URL. A subtitle
  of a video that is missing from disk MUST NOT be served. A subtitle file is hostile input: the
  server MUST open it beneath its library root never through a symbolic link, at the file or at
  any folder above it, and only as a regular file (FR-212), serving a link there, or a FIFO or
  device in its place, as missing; MUST serve length and bytes from that one open
  handle; MUST read no more than 8 MiB of it for conversion, whatever its size claims; and MUST
  convert it in time linear in its length, whatever markup it holds.
- **FR-513**: Of a title's file versions, exactly one MUST be marked primary and listed first: the
  default edition before a named one, then the tallest picture, the highest video bit rate, the
  largest file, and the lowest file id. The choice MUST be computed from the files when read, and
  the detail endpoint's `file_id` and duration MUST be the primary's. A multi-part version is ranked
  by its first part's picture and all its parts' size, and its `file_id` is its first part's.
- **FR-514**: The server MUST let a user mark a movie, an episode, a season or a whole show watched
  -- a season or show marking each of its episodes, idempotently -- or unwatched, forgetting its
  played state and position; and MUST carry the user's state on detail payloads: played, position
  and play count for a movie and each episode, and how many episodes are played for each season and
  show. It MUST expose an episode's detail (with its season, its show and the episodes either side
  of it) and a season's detail by id.

## FR-6xx — Administration

- **FR-601**: The server MUST provide an admin-only API surface for library root management (create,
  list, remove) and MUST reject these requests from non-admin authenticated users with an
  authorization error.
- **FR-602**: The server MUST provide an admin-only endpoint to trigger a library rescan, gated by
  admin role per FR-601's authorization behavior.
- **FR-603**: The server MUST provide an admin-only endpoint to trigger metadata re-enrichment
  (FR-308), gated by admin role.
- **FR-604**: The server MUST provide an admin-only endpoint or SSE stream exposing current scan
  progress, enrichment status, and recent system/admin log entries.
- **FR-605**: The web client MUST provide an admin area, visible only to users with the admin role,
  exposing library management, scan progress, enrichment status with a manual re-enrich control, and
  a log viewer.
- **FR-606**: The web client MUST NOT render admin-only navigation entries or controls to
  non-admin users, in addition to server-side authorization enforcement per FR-601–FR-604.
- **FR-607**: All admin-only mutating endpoints (library CRUD, rescan trigger, re-enrich trigger)
  MUST require both a valid authenticated session and the admin role — authentication alone MUST NOT
  be sufficient.
- **FR-608**: The server MUST provide an admin-only endpoint that returns the anonymous library
  report it would send now -- the report and the exact request body, byte for byte -- with whether a
  destination is configured and when the report was last and will next be sent, without sending
  anything (NFR-503).
- **FR-609**: The server MUST provide an admin-only endpoint that returns the playback telemetry
  counts (FR-511) summed over a caller-chosen range of UTC days (default the last 30, at most 366),
  whether or not collection is currently enabled, and MUST prune counts older than the configured
  retention (`BEAM_PLAYBACK_TELEMETRY_RETENTION_DAYS`) daily.

## FR-7xx — Client Behavior (Resume, Search, Player)

- **FR-701**: The web client MUST display a "continue watching" row on the home page, populated from
  the endpoint in FR-508, and MUST omit items that have been completed or explicitly cleared by the
  user. The server MUST let a user remove a title from continue-watching, keeping its progress, until
  the user next plays it.
- **FR-702**: The web client MUST periodically report playback position to the server during active
  playback (per FR-507) at a bounded interval, so that resume state survives a browser refresh or
  crash without an explicit "save" action.
- **FR-703**: The web client's instant search MUST debounce keystrokes before issuing a request, to
  avoid one request per keystroke against the search endpoint (FR-404).
- **FR-704**: The web client MUST present a source-quality picker in the player UI whenever a title
  has more than one indexed file version (per FR-505), and MUST default to the highest-quality
  version when no prior selection exists.
- **FR-705**: The web client MUST provide an explicit download action, distinct from the play action,
  that invokes the full-download endpoint (FR-502) rather than the streaming endpoint.
- **FR-706**: The web client MUST reflect real-time scan and enrichment progress (admin area) by
  consuming the SSE streams from FR-208/FR-309, and MUST NOT implement this via polling.
