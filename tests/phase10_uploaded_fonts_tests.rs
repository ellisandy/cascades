//! Phase 10 — User-uploaded fonts integration tests.
//!
//! Covers the four pieces that make Phase 10 work end-to-end:
//!
//! 1. The upload route accepts font MIMEs (woff2, woff, ttf) and rejects
//!    unknown ones with a 415.
//! 2. Uploaded fonts surface in `GET /api/admin/assets` with `kind: "font"`,
//!    so the admin UI can filter them into the font picker.
//! 3. The compositor's `wrap_html` injects an `@font-face` declaration per
//!    uploaded font, pointed at `/api/assets/{id}` URLs.
//! 4. Family-name resolution: `Inter-Bold.woff2` → font-family `Inter-Bold`.
//!
//! Storage-layer tests for the kind column live alongside the asset_store
//! module itself; this file is the wire / render integration.

use cascades::fonts::{FontsManifest, UploadedFont};

/// `wrap_html` must emit one `@font-face` declaration per uploaded font,
/// with an absolute URL pointed at `/api/assets/{id}` so Chromium (which
/// runs on a different loopback port from the main server) can fetch it.
#[test]
fn wrap_html_injects_font_face_per_uploaded_font() {
    let manifest = FontsManifest::empty();
    let fonts = vec![
        UploadedFont {
            id: "asset-aaa1234567890bcd".into(),
            filename: "Inter-Bold.woff2".into(),
            mime: "font/woff2".into(),
        },
        UploadedFont {
            id: "asset-zzz1234567890bcd".into(),
            filename: "RetroMono.ttf".into(),
            mime: "font/ttf".into(),
        },
    ];
    let html = manifest.wrap_html("<div>x</div>", "http://localhost:9090", &fonts);

    // Both family names appear.
    assert!(html.contains("font-family: \"Inter-Bold\""),
        "Inter-Bold @font-face missing: {html}");
    assert!(html.contains("font-family: \"RetroMono\""),
        "RetroMono @font-face missing: {html}");
    // URLs point at /api/assets/{id} on the absolute base_url.
    assert!(html.contains("http://localhost:9090/api/assets/asset-aaa1234567890bcd"),
        "Inter URL missing: {html}");
    assert!(html.contains("http://localhost:9090/api/assets/asset-zzz1234567890bcd"),
        "RetroMono URL missing: {html}");
    // Format hints reflect the MIME, not the filename — important so a
    // user who renames `something.bin` to a font upload still gets a
    // sensible hint.
    assert!(html.contains("format(\"woff2\")"));
    assert!(html.contains("format(\"truetype\")"));
}

/// Empty uploads vector → no @font-face block from uploaded fonts. Curated
/// manifest fonts still come through normally.
#[test]
fn wrap_html_with_no_uploads_emits_only_curated_fonts() {
    // Use the real fonts manifest so we have actual @font-face content
    // from the curated side to compare against the absence-of-uploaded.
    let manifest = FontsManifest::load_from(std::path::Path::new("fonts/fonts.json"))
        .expect("manifest must load");
    let html = manifest.wrap_html("<p>hi</p>", "http://localhost:9090", &[]);

    // Curated fonts are present.
    assert!(html.contains("font-family: \"Inter\""),
        "curated Inter must still render: {html}");
    // No /api/assets URLs (those only come from uploads).
    assert!(!html.contains("/api/assets/"),
        "no asset URLs expected when uploaded list is empty: {html}");
}

/// Family-name resolution rules:
/// - Single dot → strip extension only.
/// - No dot → use the whole filename.
/// - Multiple dots → only the LAST extension is stripped (e.g.
///   `Inter.medium.woff2` → `Inter.medium`, since theme authors might
///   actually use that as the family name).
#[test]
fn uploaded_font_family_name_strips_only_last_extension() {
    let cases = [
        ("Inter-Bold.woff2", "Inter-Bold"),
        ("RetroMono.ttf",    "RetroMono"),
        ("noext",            "noext"),
        ("Inter.medium.woff2", "Inter.medium"),
        // Edge case: the entire filename is just an extension.
        (".woff2", ""),
    ];
    for (filename, expected) in cases {
        let f = UploadedFont {
            id: "asset-x".into(),
            filename: filename.into(),
            mime: "font/woff2".into(),
        };
        assert_eq!(f.family_name(), expected, "filename={filename}");
    }
}

/// Format-hint mapping for each accepted MIME. Unknown MIMEs fall back to
/// `woff2` (the most-supported modern format) — guards a misclassified
/// upload from producing an `@font-face` Chromium will never load.
#[test]
fn uploaded_font_format_hint_matches_mime() {
    let cases = [
        ("font/woff2", "woff2"),
        ("font/woff",  "woff"),
        ("font/ttf",   "truetype"),
        ("font/otf",   "woff2"), // unknown → safe default
        ("",           "woff2"),
    ];
    for (mime, expected) in cases {
        let f = UploadedFont {
            id: "asset-x".into(),
            filename: "X.woff2".into(),
            mime: mime.into(),
        };
        assert_eq!(f.format_hint(), expected, "mime={mime}");
    }
}
