# Issue 283 audit: issues still to file

This directory holds the drafted issue bodies from the #283 post-drain audit. They are staged here
because the GitHub API budget ran out before the issues could be filed. The lane that resumes
`chore/283-post-drain-deslop-audit` does the following:

1. Files each body below in order with `gh issue create --repo justin13888/Beam --title TITLE --body-file FILE`.
2. Before filing a dependent body, adds its `Blocked by #N, #M` line using the numbers it got back.
3. Deletes this directory in its own commit, then opens the pull request with `PR-BODY.md` as the
   body, its `## Filed` placeholder replaced by the issue numbers. Each filed issue is returned as
   a `split:` line, `remainder` or `handoff` as the table says.

None of this directory is meant to merge.

Every body was verified against master@670b2de3b97c6e87830c347f16878be83ccb1d4c.

| Order | File | Title | Blocked by (file keys) | Return as |
|---|---|---|---|---|
| 1 | d1.md | Android: keep a restored session signed in across cold starts and return to sign-in when it expires | - | remainder |
| 2 | d2.md | Android: drain the offline progress queue at start and on reconnect | - | remainder |
| 3 | d3.md | Android: send the session and honour certificate pins only for the server's own origin | - | remainder |
| 4 | d4.md | Android: make the software-decode and cellular-download settings take effect, and report failed actions | - | remainder |
| 5 | d5.md | Server: keep internal error text out of 5xx problem details, and report a failed logout | - | remainder |
| 6 | d6.md | Server: log authentication and admin-mutation events, and enable every Beam crate in the default log filter | - | remainder |
| 7 | d7.md | Index: make enrichment ranking and due-row selection deterministic across providers and stores | - | remainder |
| 8 | d8.md | Server: allow credentialed CORS reads only from the configured web origin, and pin cookie attributes in tests | - | remainder |
| 9 | t-domain.md | Bind every beam-domain and beam-auth store to a shared contract and prove the OIDC client's security checks | - | remainder |
| 10 | t-server.md | Characterization tests for beam-server: authorization denials, NFR-205 error paths, and state assertions | - | remainder |
| 11 | pit.md | Advisory Kotlin mutation testing with PIT | - | remainder |
| 12 | kynos.md | Name an upstream getkono/kynos issue at every Kynos workaround in beam-server | - | remainder |
| 13 | t-core.md | Characterization tests for beam-client-core: multi-server, progress flush, transport failures, proptests | d1 | remainder |
| 14 | t-index.md | Characterization tests for beam-index: error paths, provider contract, no-write guards, proptests | t-domain, d7 | remainder |
| 15 | t-android.md | Characterization tests for beam-android: native bindings, DataStore stores, player, and fakes that match the core | t-core, d1, d3 | remainder |
| 16 | s-docs.md | Reconcile docs/architecture, docs/operations, docs/testing and AGENTS.md with the code | d6, d8 | remainder |
| 17 | i-server.md | Restructure beam-server: dedupe paging and cursors, move misplaced helpers, drop dead code, split oversized files | t-server, d5, d8, kynos | remainder |
| 18 | i-index.md | Restructure beam-index: split services/index.rs, unify movie and show enrichment, drop dead seams | t-index, d7 | remainder |
| 19 | i-domain.md | Restructure beam-domain, beam-auth, beam-entity and beam-migration: drop unused dependencies and dead code, split the contract files | t-domain | remainder |
| 20 | i-core.md | Restructure beam-client-core: split ffi.rs and catalog.rs, move logic out of the FFI layer, correct error docs | t-core, d1, d3 | remainder |
| 21 | i-android.md | Remove unused Android dependencies and split oversized Kotlin files | t-android, i-core, d2, d4 | remainder |
| 22 | gate.md | Add a file-length gate with a shrinking baseline | i-server, i-index, i-domain, i-core, i-android | remainder |
| 23 | spargen.md | Bump spargen once a release carries getkono/spargen#268 and #403 | (upstream) | handoff |

t-index.md already has a placeholder sentence, "Blocked by the beam-domain test issue". Replace it with the real `Blocked by #N, #M` line. In every other dependent body, insert the line just above the `Verified against` line.
