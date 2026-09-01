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
//! from the problem attributes in every file under `src/routes`, found by
//! walking the directory rather than by naming the files; the documentation
//! side from the headings of the page those URIs point at. A slug renamed on
//! one side and not the other fails here, and so does a new declaring file
//! nobody remembered to document.
//!
//! Hermetic: it reads this crate's own sources and one `include_str!`. Nothing
//! to start, nothing to reach (NFR-201).

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::routes::api_error::ERROR_BASE;

    /// Every `.rs` file under `src/routes`, read when the test runs.
    ///
    /// Walked rather than listed. `include_str!` needs a literal, so the list
    /// used to be hand-maintained -- and a hand-maintained list is a table that
    /// silently stops covering a file someone adds. If that file's codes were
    /// also undocumented, *neither* side of the comparison below would see
    /// them and it would pass while Beam published a `type` resolving to
    /// nothing: exactly the failure this test exists to catch.
    ///
    /// Still hermetic. It reads this crate's own sources through
    /// `CARGO_MANIFEST_DIR`; there is nothing to start and nothing to reach
    /// (NFR-201).
    fn declaring_sources() -> Vec<(String, String)> {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/routes");
        let mut found: Vec<(String, String)> = std::fs::read_dir(&dir)
            .expect("src/routes is readable")
            .map(|entry| entry.expect("readable directory entry").path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
            .map(|path| {
                let name = path
                    .file_name()
                    .expect("a file has a name")
                    .to_string_lossy()
                    .into_owned();
                // Comment lines are dropped before the attribute scan. A
                // doc comment is free to *show* a `#[problem(...)]` -- this
                // file's own does -- and a scanner that could not tell the
                // two apart would read the illustration as a declaration.
                let body = std::fs::read_to_string(&path)
                    .expect("readable source file")
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .collect::<Vec<_>>()
                    .join("\n");
                (name, body)
            })
            .collect();
        found.sort();
        assert!(
            !found.is_empty(),
            "no sources found under {}; the walk has stopped finding this crate",
            dir.display()
        );
        found
    }

    /// The page every `type` URI dereferences to.
    const ERROR_REFERENCE: &str =
        include_str!("../../../beam-docs/src/content/docs/reference/errors.mdx");

    /// The body of every `#[problem(...)]` attribute in `source`.
    ///
    /// Parens are balanced and string literals skipped, so a `)` inside a
    /// title cannot end the attribute early.
    fn problem_attributes(source: &str) -> Vec<&str> {
        const OPEN: &str = "#[problem(";
        let mut found = Vec::new();
        let mut rest = source;
        while let Some(index) = rest.find(OPEN) {
            let body = &rest[index + OPEN.len()..];
            let mut depth = 1usize;
            let mut in_string = false;
            let mut end = None;
            let mut chars = body.char_indices();
            while let Some((offset, character)) = chars.next() {
                match character {
                    '\\' if in_string => {
                        chars.next();
                    }
                    '"' => in_string = !in_string,
                    '(' if !in_string => depth += 1,
                    ')' if !in_string => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(offset);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(end) = end else { break };
            found.push(&body[..end]);
            rest = &body[end..];
        }
        found
    }

    /// Every `type = "..."` literal in the declaring sources, in full.
    ///
    /// Read out of the parsed attribute rather than off a line that happens to
    /// start with `type = "`: rustfmt will not reformat a one-line
    /// `#[problem(status = 400, type = "...", title = "...")]`, and a
    /// line-oriented scan skips it silently.
    fn declared_type_uris() -> Vec<(String, String)> {
        let pattern = regex::Regex::new("type\\s*=\\s*\"([^\"]+)\"").expect("a valid pattern");
        let mut found = Vec::new();
        for (name, source) in declaring_sources() {
            for attribute in problem_attributes(&source) {
                for capture in pattern.captures_iter(attribute) {
                    found.push((name.clone(), capture[1].to_owned()));
                }
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
