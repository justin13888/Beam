use std::path::{Path, PathBuf};

use proptest::prelude::*;

use super::root_relative_path;

#[test]
fn root_relative_path_never_discloses_the_library_root() {
    // (root, file path, what a client is shown)
    let cases: &[(&str, &str, &str)] = &[
        // Nested under the root: the path below it, '/'-joined.
        ("/m/films", "/m/films/A (2016)/A.mkv", "A (2016)/A.mkv"),
        // A trailing separator on the root is the same root.
        ("/m/films/", "/m/films/A (2016)/A.mkv", "A (2016)/A.mkv"),
        // Directly at the root: just the file name.
        ("/m/films", "/m/films/A.mkv", "A.mkv"),
        // A sibling directory sharing the root's prefix as a *string* is not
        // under the root; only the file name survives.
        ("/m/films", "/m/films2/x.mkv", "x.mkv"),
        // Entirely elsewhere: only the file name survives.
        ("/m/films", "/srv/other/deep/y.mp4", "y.mp4"),
        // Climbing out of the root through `..` is not under it.
        ("/m/films", "/m/films/../secret/z.mkv", "z.mkv"),
        // The root itself names no file below it.
        ("/m/films", "/m/films", "films"),
    ];

    for &(root, path, expected) in cases {
        assert_eq!(
            root_relative_path(Path::new(root), Path::new(path)),
            expected,
            "root {root:?}, path {path:?}"
        );
    }
}

/// One normal path component: never empty, never `.` or `..`, never a
/// separator.
fn component() -> impl Strategy<Value = String> {
    "[A-Za-z0-9 ()._-]{1,12}".prop_filter("not a special component", |c| c != "." && c != "..")
}

proptest! {
    /// A file under the root is shown as exactly its components below the
    /// root, joined by `/`, and never as an absolute path.
    #[test]
    fn a_file_under_the_root_is_its_components_below_it(
        root in prop::collection::vec(component(), 0..4),
        below in prop::collection::vec(component(), 1..5),
    ) {
        let root: PathBuf = std::iter::once("/".to_owned()).chain(root).collect();
        let path = below.iter().fold(root.clone(), |path, c| path.join(c));

        let shown = root_relative_path(&root, &path);

        prop_assert_eq!(&shown, &below.join("/"));
        prop_assert!(!shown.starts_with('/'));
    }

    /// Whatever the pair -- under the root or not -- the result is never
    /// absolute.
    #[test]
    fn the_result_is_never_absolute(
        root in prop::collection::vec(component(), 1..4),
        path in prop::collection::vec(component(), 1..6),
    ) {
        let root: PathBuf = std::iter::once("/".to_owned()).chain(root).collect();
        let path: PathBuf = std::iter::once("/".to_owned()).chain(path).collect();

        let shown = root_relative_path(&root, &path);

        prop_assert!(!shown.starts_with('/'));
        prop_assert!(!Path::new(&shown).is_absolute());
    }
}
