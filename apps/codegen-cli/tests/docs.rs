#![allow(missing_docs)]

use std::fs;
use std::path::Path;

#[test]
fn compatibility_matrix_references_existing_fixtures_and_tests() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let document =
        fs::read_to_string(root.join("docs/compatibility.md")).expect("read compatibility matrix");

    let references: Vec<&str> = document
        .split('`')
        .enumerate()
        .filter_map(|(index, value)| {
            (index % 2 == 1
                && ["apps/", "examples/", "tests/", "crates/"]
                    .iter()
                    .any(|prefix| value.starts_with(prefix))
                && !value.contains(char::is_whitespace))
            .then_some(value)
        })
        .collect();

    assert!(
        !references.is_empty(),
        "matrix should cite repository evidence"
    );
    for reference in references {
        assert!(
            root.join(reference).is_file(),
            "compatibility matrix references missing file: {reference}"
        );
    }
}
