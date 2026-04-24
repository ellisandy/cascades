//! Curated-font manifest — loads `fonts/fonts.json` and emits `@font-face`
//! CSS for the sidecar to render with.
//!
//! The manifest is the single source of truth shared between the HTTP
//! `/fonts/*` route, this builder, and the admin UI font picker.

use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct FontsManifest {
    pub families: Vec<FontFamily>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FontFamily {
    pub name: String,
    pub slug: String,
    pub css_stack: String,
    pub category: String,
    pub files: Vec<FontFile>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct FontFile {
    pub path: String,
    pub weight: u16,
    pub style: String,
}

#[derive(Debug, thiserror::Error)]
pub enum FontsError {
    #[error("read manifest: {0}")]
    Read(#[from] std::io::Error),
    #[error("parse manifest: {0}")]
    Parse(#[from] serde_json::Error),
}

impl FontsManifest {
    pub fn load_from(path: &Path) -> Result<Self, FontsError> {
        let raw = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }

    /// Empty manifest — tests that don't care about `@font-face` output can
    /// pass this to `Compositor::new` without reading the real file.
    pub fn empty() -> Self {
        Self { families: Vec::new() }
    }

    /// Build the `@font-face` block that Chromium can consume. URLs are made
    /// absolute against `base_url` so the headless browser (which may be on
    /// a different loopback port) can fetch the files.
    pub fn to_font_face_css(&self, base_url: &str) -> String {
        let trimmed = base_url.trim_end_matches('/');
        let mut out = String::new();
        for family in &self.families {
            for file in &family.files {
                out.push_str("@font-face {\n");
                out.push_str(&format!("  font-family: \"{}\";\n", family.name));
                out.push_str(&format!("  font-weight: {};\n", file.weight));
                out.push_str(&format!("  font-style: {};\n", file.style));
                out.push_str(&format!(
                    "  src: url(\"{trimmed}/fonts/{}\") format(\"woff2\");\n",
                    file.path
                ));
                out.push_str("  font-display: swap;\n");
                out.push_str("}\n");
            }
        }
        out
    }

    /// Wrap inner HTML in a full document with `<head><style>@font-face…</style></head>`.
    /// The sidecar passes the result to Puppeteer with `waitUntil: networkidle0`,
    /// which blocks on font fetches — so curated fonts are guaranteed applied
    /// before the screenshot.
    pub fn wrap_html(&self, inner_html: &str, base_url: &str) -> String {
        let face_css = self.to_font_face_css(base_url);
        format!(
            "<!DOCTYPE html>\
             <html><head><meta charset=\"utf-8\"><style>\
             {face_css}\
             html,body{{margin:0;padding:0;}}\
             </style></head><body>{inner_html}</body></html>"
        )
    }
}
