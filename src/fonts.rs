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

    /// Wrap inner HTML in a full document with `<head><style>…</style></head>`.
    /// The sidecar passes the result to Puppeteer with `waitUntil: networkidle0`,
    /// which blocks on font fetches — so all fonts are guaranteed applied
    /// before the screenshot.
    ///
    /// Style block contents (in order):
    /// 1. **`@font-face`** declarations from the curated manifest.
    /// 2. **`@font-face`** declarations for user-uploaded fonts (Phase 10),
    ///    pointing at `/api/assets/{id}` URLs.
    /// 3. **Base utility CSS** ([`BASE_CSS`]) — hand-rolled flex-layout +
    ///    typographic-scale rules used by every plugin template. Without
    ///    this every template would render as unstyled HTML — the utility
    ///    classes (`flex--col`, `value--xxxlarge`, etc.) would have no
    ///    rules backing them.
    ///
    /// `uploaded_fonts` is the list of font-typed assets from the
    /// AssetStore; pass an empty slice to skip the second block. Compositor
    /// builds this list once per render via `AssetStore::list_fonts`.
    pub fn wrap_html(
        &self,
        inner_html: &str,
        base_url: &str,
        uploaded_fonts: &[UploadedFont],
    ) -> String {
        let curated_face_css = self.to_font_face_css(base_url);
        let uploaded_face_css = uploaded_font_face_css(uploaded_fonts, base_url);
        format!(
            "<!DOCTYPE html>\
             <html><head><meta charset=\"utf-8\"><style>\
             {curated_face_css}\
             {uploaded_face_css}\
             {BASE_CSS}\
             </style></head><body>{inner_html}</body></html>"
        )
    }
}

/// Phase 10: a user-uploaded font asset, in the minimal shape `wrap_html`
/// needs to emit a valid `@font-face` declaration. Built by the compositor
/// from `AssetSummary`s where `kind == "font"`.
#[derive(Debug, Clone)]
pub struct UploadedFont {
    /// Asset id used in the URL: `/api/assets/{id}`.
    pub id: String,
    /// Filename — used both as the `font-family` name (after extension
    /// strip) and to pick the right `format(...)` hint from the extension.
    pub filename: String,
    /// MIME from the upload, e.g. `"font/woff2"`. Used to derive the
    /// `format(...)` hint independent of filename in case the user
    /// uploaded a `.woff2` named `Whatever.bin`.
    pub mime: String,
}

impl UploadedFont {
    /// CSS `font-family` name derived from the upload filename. Strips the
    /// extension; replaces nothing else (so `Inter-Bold.woff2` stays
    /// `Inter-Bold` rather than collapsing to `Inter`). Plugin authors
    /// reference the same string in `font-family:` declarations.
    pub fn family_name(&self) -> &str {
        match self.filename.rfind('.') {
            Some(dot) => &self.filename[..dot],
            None => &self.filename,
        }
    }

    /// CSS `format(...)` hint matching the asset's MIME. Chromium uses the
    /// hint to skip incompatible sources without fetching them. Unknown
    /// MIME → `"woff2"` as the safest default (it's the most-supported
    /// modern format).
    pub fn format_hint(&self) -> &'static str {
        match self.mime.as_str() {
            "font/woff2" => "woff2",
            "font/woff" => "woff",
            "font/ttf" => "truetype",
            _ => "woff2",
        }
    }
}

fn uploaded_font_face_css(fonts: &[UploadedFont], base_url: &str) -> String {
    if fonts.is_empty() {
        return String::new();
    }
    let trimmed = base_url.trim_end_matches('/');
    let mut out = String::new();
    for f in fonts {
        out.push_str("@font-face {\n");
        out.push_str(&format!("  font-family: \"{}\";\n", f.family_name()));
        // Uploaded fonts don't (yet) declare weight/style — we'd need a
        // separate inspector flow for that. Default to normal/normal so
        // the @font-face is well-formed and the user can apply the family
        // by name without picking a weight.
        out.push_str("  font-weight: normal;\n");
        out.push_str("  font-style: normal;\n");
        out.push_str(&format!(
            "  src: url(\"{trimmed}/api/assets/{}\") format(\"{}\");\n",
            f.id,
            f.format_hint()
        ));
        out.push_str("  font-display: swap;\n");
        out.push_str("}\n");
    }
    out
}

/// Hand-rolled utility CSS injected into every sidecar render. Source of
/// truth lives next to the templates so it's editable as plain CSS, but
/// gets baked into the binary at compile time so the loader never has to
/// touch the filesystem at render time.
///
/// See `templates/_base.css` for the actual rules. If you add a new
/// utility class to a template, add the matching rule there.
const BASE_CSS: &str = include_str!("../templates/_base.css");

#[cfg(test)]
mod tests {
    use super::*;

    /// `wrap_html` must inline the base CSS so utility classes (`flex--col`,
    /// `value--xxxlarge`, etc.) actually have rules backing them. Without
    /// this every plugin template would render as unstyled HTML at the
    /// sidecar.
    #[test]
    fn wrap_html_inlines_base_css_utility_classes() {
        let manifest = FontsManifest::empty();
        let wrapped = manifest.wrap_html("<div>hi</div>", "http://example/", &[]);
        // Sample a handful of classes to confirm the include_str! actually
        // landed in the output. Spot-checking is enough — if any rule
        // shows up the file was loaded.
        assert!(wrapped.contains(".flex--col"),
            "missing .flex--col rule in wrapped output");
        assert!(wrapped.contains(".value--xxxlarge"),
            "missing .value--xxxlarge rule");
        assert!(wrapped.contains(".title_bar"),
            "missing .title_bar rule");
        // The original payload is still wrapped intact.
        assert!(wrapped.contains("<div>hi</div>"),
            "inner_html must round-trip into the body");
    }

    /// Every class actually used in `templates/*.html.jinja` must have at
    /// least one matching rule somewhere — either in `templates/_base.css`
    /// (utility classes) or in an inline `<style>` block in the same file
    /// the class is used in (plugin-local CSS-variable hosts and the like).
    /// Catches the "added a new class to a template, forgot to add a rule"
    /// failure mode at test-time rather than at render-time-on-device.
    #[test]
    fn every_template_class_has_a_matching_css_rule() {
        let templates_dir = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/templates"
        ));

        // Two passes per template: collect classes USED, and concatenate any
        // inline <style> blocks. The class is satisfied if any block (this
        // template's own style OR base.css) contains a `.{class}` rule.
        let mut violations: Vec<(String, String)> = Vec::new();
        for entry in std::fs::read_dir(templates_dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jinja") {
                continue;
            }
            let src = std::fs::read_to_string(&path).unwrap();
            let template_name = path.file_name().unwrap().to_string_lossy().to_string();

            // Concatenate every inline <style>...</style> block in this
            // template (templates may have more than one, e.g. for theming
            // CSS variables vs. pure layout overrides).
            let mut local_styles = String::new();
            for chunk in src.split("<style>").skip(1) {
                if let Some(end) = chunk.find("</style>") {
                    local_styles.push_str(&chunk[..end]);
                }
            }

            // Collect classes USED via class="..." attributes.
            let mut classes_used = std::collections::HashSet::new();
            for chunk in src.split("class=\"").skip(1) {
                if let Some(end) = chunk.find('"') {
                    for cls in chunk[..end].split_whitespace() {
                        classes_used.insert(cls.to_string());
                    }
                }
            }

            for cls in classes_used {
                let needle = format!(".{cls}");
                let satisfied = BASE_CSS.contains(&needle) || local_styles.contains(&needle);
                if !satisfied {
                    violations.push((template_name.clone(), cls));
                }
            }
        }
        violations.sort();
        assert!(
            violations.is_empty(),
            "templates use classes with no matching CSS rule (in base.css OR \
             a local <style> block): {violations:?}\n\
             Add rules to templates/_base.css for utility classes, or to a \
             <style> block in the template itself for plugin-local classes."
        );
    }
}
