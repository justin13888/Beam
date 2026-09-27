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
//! the server emitted every one of them (issue #123).
//!
//! Neither side of the comparison is hand-maintained, which is what keeps this
//! from being the forbidden second copy of a table. The code side is read from
//! the document `create_router` exports: kynos narrows a problem response to
//! the `type` values its operation can emit, whether a variant's
//! `#[problem(type = ...)]`, a scope set's `FORBIDDEN_TYPE` or a limiter's
//! `ProblemType` declared it. (A response Kynos keeps wide on purpose -- the
//! `SessionAuth` 403, the range 416 -- names no code, and no Beam code is
//! emitted only there.) The documentation side is the headings of the
//! page those URIs point at. A slug renamed on one side and not the other fails
//! here, and so does a new code nobody remembered to document.
//!
//! Hermetic: it builds the router's description and reads one `include_str!`.
//! Nothing to start, nothing to reach (NFR-201).

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use kynos::openapi::SpecVersion;

    use crate::routes::api_error::ERROR_BASE;
    use crate::routes::create_router;

    /// The page every `type` URI dereferences to.
    const ERROR_REFERENCE: &str =
        include_str!("../../../beam-docs/src/content/docs/reference/errors.mdx");

    /// RFC 9457's "the status code is the whole story", which names no section.
    const ABOUT_BLANK: &str = "about:blank";

    /// Every `type` the exported document says a problem may carry, beside the
    /// JSON pointer it was found at.
    fn declared_type_uris() -> Vec<(String, String)> {
        fn walk(node: &serde_json::Value, pointer: &str, found: &mut Vec<(String, String)>) {
            match node {
                serde_json::Value::Object(object) => {
                    if let Some(uri) = object
                        .get("properties")
                        .and_then(|properties| properties.get("type"))
                        .and_then(|member| member.get("const"))
                        .and_then(serde_json::Value::as_str)
                    {
                        found.push((pointer.to_owned(), uri.to_owned()));
                    }
                    for (key, child) in object {
                        let token = key.replace('~', "~0").replace('/', "~1");
                        walk(child, &format!("{pointer}/{token}"), found);
                    }
                }
                serde_json::Value::Array(items) => {
                    for (index, child) in items.iter().enumerate() {
                        walk(child, &format!("{pointer}/{index}"), found);
                    }
                }
                _ => {}
            }
        }

        let document = serde_json::to_value(
            create_router()
                .openapi_as(SpecVersion::V3_2)
                .expect("the router exports"),
        )
        .expect("the document serializes");
        let mut found = Vec::new();
        walk(&document, "#", &mut found);
        found.retain(|(_, uri)| uri != ABOUT_BLANK);
        assert!(
            !found.is_empty(),
            "no problem `type` found in the document; the walk has stopped matching it"
        );
        found
    }

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
