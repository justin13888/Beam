//! The problem-type taxonomy, asserted against the page it is published on.
//!
//! Every `type` Beam emits is a URI a client is invited to branch on and a
//! reader is invited to follow. Two things have to hold for that to be true,
//! and neither is checked by anything else:
//!
//! * every code shares [`ERROR_BASE`], so one typo cannot publish an
//!   identifier under an origin nobody serves;
//! * every code has a section on the error reference, and the reference
//!   describes no code the server cannot emit.
//!
//! The second is the one that actually broke. `docs/architecture/api.md` and
//! `beam-docs`' `reference/errors` are both prose, and the page spent the whole
//! of the Kynos migration asserting that Beam had no error codes at all while
//! the server emitted twenty-two of them (issue #123).
//!
//! Neither side of the comparison is hand-maintained, which is what keeps this
//! from being the forbidden second copy of a table. The code side is extracted
//! from the `#[problem(type = ...)]` attributes in the files that declare them;
//! the documentation side from the headings of the page those URIs point at. A
//! slug renamed on one side and not the other fails here.
//!
//! Hermetic: two `include_str!`s resolved at compile time. No filesystem, no
//! network, nothing to run first (NFR-201).

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::routes::api_error::ERROR_BASE;

    /// The sources that declare a problem type.
    ///
    /// Listed rather than globbed because `include_str!` needs a literal, and
    /// a file added here without a line added there is caught by the
    /// documentation comparison below rather than passing silently.
    const DECLARING_SOURCES: [(&str, &str); 4] = [
        ("routes/api_error.rs", include_str!("api_error.rs")),
        ("routes/auth.rs", include_str!("auth.rs")),
        ("routes/middleware.rs", include_str!("middleware.rs")),
        ("routes/metrics_mw.rs", include_str!("metrics_mw.rs")),
    ];

    /// The page every `type` URI dereferences to.
    const ERROR_REFERENCE: &str =
        include_str!("../../../beam-docs/src/content/docs/reference/errors.mdx");

    /// Every `type = "..."` literal in the declaring sources, in full.
    fn declared_type_uris() -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        for (name, source) in DECLARING_SOURCES {
            for line in source.lines() {
                let line = line.trim();
                let Some(rest) = line.strip_prefix("type = \"") else {
                    continue;
                };
                let Some(uri) = rest.strip_suffix("\",").or_else(|| rest.strip_suffix('"')) else {
                    continue;
                };
                found.push((name, uri.to_owned()));
            }
        }
        assert!(
            !found.is_empty(),
            "no `type = \"...\"` attributes found; the extraction has stopped matching the source"
        );
        found
    }

    /// The codes the server can emit.
    fn declared_codes() -> BTreeSet<String> {
        declared_type_uris()
            .into_iter()
            .filter_map(|(_, uri)| uri.strip_prefix(ERROR_BASE).map(str::to_owned))
            .collect()
    }

    /// The codes the reference documents, from the headings its anchors come
    /// from.
    ///
    /// Starlight slugs a heading from its own text, so a section whose heading
    /// *is* the code anchors at exactly that code. Writing them that way is
    /// what lets this comparison exist without a plugin and without coupling a
    /// published URI to a sentence someone may reword.
    fn documented_codes() -> BTreeSet<String> {
        ERROR_REFERENCE
            .lines()
            .filter_map(|line| line.strip_prefix("### "))
            .map(str::trim)
            .filter(|heading| {
                heading
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            })
            .map(str::to_owned)
            .collect()
    }

    /// One typo would publish an identifier under an origin nobody serves.
    #[test]
    fn every_problem_type_hangs_under_the_published_base() {
        let stray: Vec<_> = declared_type_uris()
            .into_iter()
            .filter(|(_, uri)| !uri.starts_with(ERROR_BASE))
            .collect();

        assert!(
            stray.is_empty(),
            "these problem types do not start with ERROR_BASE ({ERROR_BASE}): {stray:#?}"
        );
    }

    /// The base has to be anchor-shaped, or every code resolves to a path that
    /// does not exist -- which is exactly what it used to do.
    #[test]
    fn the_published_base_addresses_a_fragment() {
        assert!(
            ERROR_BASE.ends_with('#'),
            "ERROR_BASE must end with `#` so each code is a section of one page, not a path \
             under a directory that has never been served: {ERROR_BASE}"
        );
    }

    /// The code and the page it points at describe the same set.
    #[test]
    fn every_problem_type_has_a_published_section() {
        let declared = declared_codes();
        let documented = documented_codes();

        let undocumented: Vec<_> = declared.difference(&documented).collect();
        let unreachable: Vec<_> = documented.difference(&declared).collect();

        assert!(
            undocumented.is_empty() && unreachable.is_empty(),
            "the taxonomy and its published reference disagree.\n\
             \n\
             emitted by beam-server, missing a `### <code>` section in \
             beam-docs/src/content/docs/reference/errors.mdx:\n  {undocumented:?}\n\
             \n\
             documented there, but no longer emitted by beam-server:\n  {unreachable:?}"
        );
    }
}
