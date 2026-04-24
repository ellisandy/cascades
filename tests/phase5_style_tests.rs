//! Phase 5 — style plumbing tests.
//!
//! Each test here maps to one red-green step in the Phase 5 implementation
//! (see `docs/plugin-customization-design.md`). Tests are added incrementally.

use std::path::Path;

/// Red 1: the curated-fonts manifest and its referenced woff2 files must exist
/// on disk. This is the single source of truth consumed by both the Rust
/// `/fonts/*` route and the sidecar's `@font-face` wrapper, so "files are on
/// disk and non-empty" is the cheapest guard against a half-committed font set.
#[test]
fn fonts_manifest_and_referenced_files_exist() {
    let manifest_path = Path::new("fonts/fonts.json");
    assert!(
        manifest_path.exists(),
        "fonts/fonts.json must exist — it's the manifest consumed by both the \
         server's /fonts/* route and the sidecar's @font-face builder"
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(manifest_path)
            .expect("fonts/fonts.json must be readable"),
    )
    .expect("fonts/fonts.json must parse as JSON");

    let families = manifest["families"]
        .as_array()
        .expect("manifest must have a top-level `families` array");
    assert_eq!(
        families.len(),
        5,
        "Phase 5 ships 5 curated families (Inter, IBM Plex Sans, DM Serif \
         Display, JetBrains Mono, Space Grotesk)"
    );

    let expected_names = [
        "Inter",
        "IBM Plex Sans",
        "DM Serif Display",
        "JetBrains Mono",
        "Space Grotesk",
    ];
    let actual_names: Vec<&str> = families
        .iter()
        .map(|f| f["name"].as_str().expect("family.name is required"))
        .collect();
    for name in expected_names {
        assert!(
            actual_names.contains(&name),
            "family {name:?} missing from manifest; have {actual_names:?}"
        );
    }

    for family in families {
        let name = family["name"].as_str().unwrap();
        let files = family["files"]
            .as_array()
            .unwrap_or_else(|| panic!("family {name:?} must have `files` array"));
        assert!(
            !files.is_empty(),
            "family {name:?} must declare at least one font file"
        );
        for file in files {
            let rel_path = file["path"]
                .as_str()
                .unwrap_or_else(|| panic!("family {name:?} file.path must be a string"));
            let full_path = Path::new("fonts").join(rel_path);
            assert!(
                full_path.exists(),
                "font file referenced in manifest is missing: {full_path:?}"
            );
            let size = std::fs::metadata(&full_path)
                .unwrap_or_else(|e| panic!("cannot stat {full_path:?}: {e}"))
                .len();
            assert!(
                size > 1000,
                "font file {full_path:?} is suspiciously small ({size} bytes) \
                 — likely a broken download"
            );
        }
    }
}
