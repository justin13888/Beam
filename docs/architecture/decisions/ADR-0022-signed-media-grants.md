# ADR-0022: Signed, file-scoped media grants for clients without the session cookie

## Status

Accepted. Amends FR-504 and NFR-102, and refines [ADR-0003](ADR-0003-oidc-bff-auth.md) for the
four delivery operations only. Additive under [ADR-0021](ADR-0021-v1-frozen.md). Settles
[#461](https://github.com/justin13888/beam/issues/461).

## Context

`/v1/files/{file_id}/stream` and `/download` accepted the `beam_session` cookie and nothing else
(FR-504), and NFR-102 kept every credential out of every URL. That holds for a client that makes
its own requests: a browser's `<video>` element sends the cookie, and `beam-client-core` hands
Media3 the cookie as a header.

It fails for a client Beam does not drive. An AirPlay video handoff, a Cast receiver, or an
"open in..." external player such as VLC is handed a URL and fetches it itself, with no cookie and
no way to be given one. The Apple player already advertises AirPlay. Each of these is refused with
a 401.

Handing such a client the session cookie in a URL would put a credential that lasts days, and
opens the whole API, into receiver logs, player histories and `Referer` headers. A grant has to be
worth far less to whoever reads it.

## Decision

**A grant is a signed capability for one file, issued to a signed-in session.**
`POST /v1/files/{file_id}/grant` (session cookie only) answers `stream_url`, `download_url` and
`expires_at`. Both URLs are the ordinary delivery paths with a `grant` query parameter, relative to
the server's origin as every source URL is.

- **Token.** `file_id || user_id || expiry || tag`, base64url without padding, where `tag` is
  HMAC-SHA256 over a domain label and the first three fields. Verifying the signature reads no
  store, and the tag is checked in constant time before the expiry.
- **Key.** 32 random bytes drawn when the server builds its state, never persisted or configured.
  A restart voids every grant outstanding, which is also the only way to revoke them all; Beam
  runs exactly one server replica (ADR-0018), so no second process needs the key.
- **Lifetime: four hours.** The issue proposed minutes. But a receiver fetches the file range by
  range for as long as it plays, and every range is checked against the expiry, so a grant that
  lapsed mid-film would stop playback with no way for the receiver to recover. Four hours covers
  nearly every film with its pauses, and is still a sliver of a session's lifetime. A longer title
  takes a fresh grant to restart from its resume point.
- **Scope.** The grant is declared as its own Kynos security scheme, `MediaGrant`
  (`apiKey` in `query`, `grant`), and the four delivery operations alone take
  `Auth<AnyOf<(SessionCookie, MediaGrant)>>`. Every other operation names only the cookie, so a
  grant authenticates nothing else -- the document says so, and the router enforces what the
  document says (ADR-0010, CLAUDE.md rule 3). Kynos 0.4.0's any-of security, which ADR-0017 noted,
  is what makes this a declaration rather than a hand-written check.
- **The file.** A grant for one file presented on another file's path is a 404, the answer a file
  the user may not see gets, so a grant reveals nothing about ids it does not name.
- **Access.** The grant acts for its user under their access policy, checked on every request
  exactly as a session's is. A file the user may not see cannot be granted (404), and a title
  hidden after a grant was issued is hidden from it from that request on.
- **A disabled user.** Disabling an account revokes its sessions so that it cannot act again until
  re-enabled (`PATCH /v1/admin/users/{id}`, issue #85). A grant is held to that too: every use
  reads the user row, and a grant of a disabled or deleted user is a 401 -- one primary-key read
  per request a grant makes, against a stateless check that would let a disabled account keep
  fetching for four hours. A grant of a re-enabled user works again until it expires, as the
  account does. A failed read is a 503, which `MediaGrant` declares, as `BeamSession` does for
  its store.
- **Both credentials.** A request carrying the session cookie and a grant is the session's: the
  cookie is tried first, so a browser that holds both acts as the session it signed in as.

## Consequences

- FR-504 now admits a media grant on the delivery operations, and NFR-102 excepts it: the one
  credential allowed in a URL is one that names one file, expires within hours, and opens nothing
  else. The session cookie stays out of every URL.
- `/v1` gains one operation and one security alternative on four existing ones. Both are additive
  under ADR-0021: a client that never asks for a grant sees no change.
- A leaked grant URL lets its reader fetch that one file until it expires, under the issuing
  user's policy, unless the user is disabled first. That is the residual risk this ADR accepts;
  there is no per-grant revocation, because a store of grants would cost the signature check its
  point.
- `beam-client-core` exposes the grant as `BeamClient::media_grant`, resolving both URLs against
  the server's origin, so a platform can hand an absolute URL to AirPlay, Cast or an external
  player.
