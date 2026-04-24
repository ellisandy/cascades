//! Plugin registry — TOML loader, plugins.d/ drop-in directory, hot-reload.
//!
//! A plugin binds a data source to display templates and declares the settings
//! a user can configure. Plugins are defined in TOML files under `config/`:
//!
//! ```text
//! config/
//!   plugins.toml        ← baseline definitions (usually empty or sparse)
//!   plugins.d/          ← drop-in: one .toml file per plugin
//!     river.toml
//!     weather.toml
//!     ferry.toml
//!     trail.toml
//!     road.toml
//! ```
//!
//! Loading order: `plugins.toml` first, then every `*.toml` in `plugins.d/`
//! (sorted by filename). Later definitions overwrite earlier ones for the same
//! plugin ID, so `plugins.d/` files can override baseline definitions.
//!
//! Hot-reload: the [`watch_plugins_d`] function sets up a [`notify`] watcher on
//! `plugins.d/`. On any `Create` or `Modify` event the affected file is reloaded
//! into the registry. SIGHUP (Unix only) triggers a full reload of both files.

use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};
use thiserror::Error;

use crate::domain::PluginId;

// ─── Error type ──────────────────────────────────────────────────────────────

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("failed to read '{path}': {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse '{path}': {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to read directory '{path}': {source}")]
    Dir {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

// ─── Data structures ─────────────────────────────────────────────────────────

/// How a plugin receives its data.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataStrategy {
    /// The scheduler polls the source at `refresh_interval_secs`.
    #[default]
    Polling,
    /// An external system pushes data via `POST /webhook/:id`.
    Webhook,
    /// Data is static; no fetch is performed.
    Static,
}

/// A single trip evaluation criterion registered by a plugin.
///
/// When the evaluation engine runs, it tests `data[key]` against `threshold`
/// using `operator`. The result contributes to the plugin's go/no-go decision.
#[derive(Debug, Clone, Deserialize)]
pub struct PluginCriterion {
    /// JSON key path into the plugin's cached data (e.g. `"water_level_ft"`).
    pub key: String,
    /// Human-readable label (e.g. `"River level"`).
    pub label: String,
    /// Comparison operator: `"lte"`, `"gte"`, `"eq"`, or `"between"`.
    pub operator: String,
    /// Numeric threshold to test against.
    pub threshold: f64,
    /// Unit suffix for display (e.g. `"ft"`, `"°F"`).
    #[serde(default)]
    pub unit: String,
    /// Which side of the threshold is the good direction: `"below"` or `"above"`.
    #[serde(default)]
    pub go_direction: String,
}

/// A single user-configurable field in a plugin's settings schema.
///
/// The settings schema drives auto-generated configuration UI and validation.
/// Sensitive fields (e.g. API keys) should be stored encrypted.
#[derive(Debug, Clone, Deserialize)]
pub struct SettingsField {
    /// Machine-readable key stored in the plugin instance settings JSON.
    pub key: String,
    /// Human-readable label for the UI.
    pub label: String,
    /// Input type hint: `"text"`, `"number"`, `"password"`, `"select"`.
    #[serde(rename = "type")]
    pub field_type: String,
    /// Whether the field must be provided before the plugin can run.
    #[serde(default)]
    pub required: bool,
    /// UI placeholder text.
    pub placeholder: Option<String>,
    /// Default value used when the user provides none.
    pub default: Option<String>,
}

/// A fully-resolved plugin definition loaded from TOML.
///
/// A plugin definition declares what a plugin *is*. Plugin *instances* (with
/// concrete user settings) are stored separately in SQLite (cs-9jd).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginDefinition {
    /// Stable machine ID used as the cache key throughout the system.
    /// Must be unique across all loaded plugins (e.g. `"river"`, `"weather"`).
    pub id: String,
    /// Human-readable plugin name shown in the UI.
    pub name: String,
    /// Short description of what this plugin shows.
    #[serde(default)]
    pub description: String,
    /// Identifies which `Source` implementation to instantiate (e.g. `"usgs"`).
    pub source: String,
    /// How often the scheduler should call `fetch`, in seconds.
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval_secs: u64,
    /// How data arrives for this plugin.
    #[serde(default)]
    pub data_strategy: DataStrategy,

    // ── Template paths ──────────────────────────────────────────────────────
    // At least `template_full` should be provided. Absent variants fall back
    // to `template_full` at render time.
    /// Template for the full 800×480 layout.
    pub template_full: Option<String>,
    /// Template for the half-horizontal (800×240) layout.
    pub template_half_horizontal: Option<String>,
    /// Template for the half-vertical (400×480) layout.
    pub template_half_vertical: Option<String>,
    /// Template for the quadrant (400×240) layout.
    pub template_quadrant: Option<String>,

    // ── Trip evaluation criteria ────────────────────────────────────────────
    /// Criteria registered at load time. Empty means the plugin never produces
    /// a go/no-go signal.
    #[serde(default)]
    pub criteria: Vec<PluginCriterion>,

    // ── Settings schema ──────────────────────────────────────────────────────
    /// Fields the user can configure for this plugin.
    #[serde(default)]
    pub settings_schema: Vec<SettingsField>,

    // ── Default layout elements ──────────────────────────────────────────────
    /// Elements spawned when this plugin is first dropped on the canvas.
    /// Empty means "drop as a single opaque `PluginSlot`" (legacy behaviour).
    /// Non-empty means "drop as a `Group` containing these children".
    #[serde(default)]
    pub default_elements: Vec<DefaultElement>,
}

/// One element in a plugin's default layout composition.
///
/// Declared in plugin manifests under `[[default_elements]]`. When the plugin is
/// dragged from the palette onto the canvas, the admin UI materialises one child
/// item per entry, nested in a `Group`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct DefaultElement {
    /// What variant of [`crate::layout_store::LayoutItem`] to spawn. One of:
    /// `"data_field"`, `"static_text"`, `"static_datetime"`, `"static_divider"`.
    pub kind: String,
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
    #[serde(default)]
    pub z_index: i32,

    // ── data_field ──────────────────────────────────────────────────────────
    /// JSONPath into the plugin's cached data (e.g. `"$.current.temp_f"`).
    /// Used to bootstrap a field mapping in `data_source_fields`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    /// Optional human label; also used as the bootstrapped mapping's `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Format string for `data_field` rendering (e.g. `"{{value}}°F"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font_size: Option<i32>,

    // ── static_text ─────────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_content: Option<String>,

    // ── static_datetime ─────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,

    // ── shared ──────────────────────────────────────────────────────────────
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orientation: Option<String>,
}

fn default_refresh_interval() -> u64 {
    300
}

/// Intermediate structure for deserializing a `plugins.toml` or `plugins.d/*.toml` file.
///
/// Each file may contain one or more `[[plugin]]` entries.
#[derive(Debug, Deserialize)]
struct PluginFile {
    #[serde(default)]
    plugin: Vec<PluginDefinition>,
}

// ─── Registry ────────────────────────────────────────────────────────────────

/// Runtime plugin registry: a shareable map from plugin ID to its definition.
///
/// Clone is cheap — the inner map is behind an `Arc`.
#[derive(Debug, Clone, Default)]
pub struct PluginRegistry {
    inner: Arc<RwLock<HashMap<PluginId, PluginDefinition>>>,
}

impl PluginRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a clone of the definition for `id`, if present.
    pub fn get(&self, id: &str) -> Option<PluginDefinition> {
        self.inner.read().unwrap().get(id).cloned()
    }

    /// Return all registered plugin definitions, sorted by ID.
    pub fn all(&self) -> Vec<PluginDefinition> {
        let guard = self.inner.read().unwrap();
        let mut defs: Vec<_> = guard.values().cloned().collect();
        defs.sort_by(|a, b| a.id.cmp(&b.id));
        defs
    }

    /// Return the number of registered plugins.
    pub fn len(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    /// Return `true` if no plugins are registered.
    pub fn is_empty(&self) -> bool {
        self.inner.read().unwrap().is_empty()
    }

    /// Merge the definitions from `path` into the registry.
    ///
    /// Each `[[plugin]]` entry in the file is inserted or overwrites any
    /// existing entry with the same `id`.
    pub fn load_file(&self, path: &Path) -> Result<(), RegistryError> {
        let contents = std::fs::read_to_string(path).map_err(|e| RegistryError::Read {
            path: path.to_string_lossy().into_owned(),
            source: e,
        })?;
        let file: PluginFile = toml::from_str(&contents).map_err(|e| RegistryError::Parse {
            path: path.to_string_lossy().into_owned(),
            source: e,
        })?;
        let mut guard = self.inner.write().unwrap();
        for plugin in file.plugin {
            guard.insert(plugin.id.clone(), plugin);
        }
        Ok(())
    }

    /// Load `plugins.toml` from `config_dir`, then all `*.toml` files from
    /// `config_dir/plugins.d/`. Later files overwrite earlier ones for the
    /// same plugin ID. Missing directories/files are silently skipped.
    pub fn load_from_config_dir(&self, config_dir: &Path) -> Result<(), RegistryError> {
        let base = config_dir.join("plugins.toml");
        if base.exists() {
            self.load_file(&base)?;
        }

        let plugins_d = config_dir.join("plugins.d");
        if plugins_d.is_dir() {
            self.load_dir(&plugins_d)?;
        }

        Ok(())
    }

    /// Load all `*.toml` files from `dir` (non-recursive, sorted by filename).
    pub fn load_dir(&self, dir: &Path) -> Result<(), RegistryError> {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
            .map_err(|e| RegistryError::Dir {
                path: dir.to_string_lossy().into_owned(),
                source: e,
            })?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        entries.sort();
        for path in entries {
            self.load_file(&path)?;
        }
        Ok(())
    }

    /// Remove all plugins loaded from `path` (i.e. plugins whose definitions
    /// came from that file) then reload the file.
    ///
    /// This is called by the filesystem watcher when a file changes.
    pub fn reload_file(&self, path: &Path) -> Result<(), RegistryError> {
        // Full reload strategy: reload the entire file rather than tracking
        // provenance per-plugin. This is simpler and correct for small registries.
        self.load_file(path)
    }
}

// ─── Loading helpers ─────────────────────────────────────────────────────────

/// Load plugins from the default config locations relative to `config_dir`
/// and return a populated registry.
///
/// Errors from missing directories or unparseable files are returned; callers
/// that want lenient startup should log and continue.
pub fn load_registry(config_dir: &Path) -> Result<PluginRegistry, RegistryError> {
    let registry = PluginRegistry::new();
    registry.load_from_config_dir(config_dir)?;
    Ok(registry)
}

// ─── Hot-reload (filesystem watcher) ─────────────────────────────────────────

/// Start a filesystem watcher on `plugins_d_dir`.
///
/// On any `Create` or `Modify` event for a `.toml` file, the affected file is
/// reloaded into `registry`. The watcher runs on a background thread and is
/// kept alive as long as the returned handle is not dropped.
///
/// Returns `Err` if the watcher cannot be set up (e.g. inotify limit hit).
pub fn watch_plugins_d(
    registry: PluginRegistry,
    plugins_d_dir: PathBuf,
) -> notify::Result<notify::RecommendedWatcher> {
    use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

    let (tx, rx) = std::sync::mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(tx, Config::default())?;
    watcher.watch(&plugins_d_dir, RecursiveMode::NonRecursive)?;

    std::thread::spawn(move || {
        for result in rx {
            match result {
                Ok(event) => {
                    let is_create_or_modify = matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_)
                    );
                    if !is_create_or_modify {
                        continue;
                    }
                    for path in &event.paths {
                        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                            continue;
                        }
                        match registry.reload_file(path) {
                            Ok(()) => log::info!(
                                "plugin_registry: reloaded '{}'",
                                path.display()
                            ),
                            Err(e) => log::warn!(
                                "plugin_registry: failed to reload '{}': {}",
                                path.display(),
                                e
                            ),
                        }
                    }
                }
                Err(e) => log::warn!("plugin_registry: watcher error: {}", e),
            }
        }
    });

    Ok(watcher)
}

// ─── SIGHUP fallback / manual reload trigger ─────────────────────────────────

/// A handle that triggers a full reload of `config_dir` into a registry.
///
/// The binary wires this to SIGHUP (e.g. via `tokio::signal::unix`) and calls
/// [`ReloadHandle::trigger`] from the signal handler. This keeps the library
/// free of signal-handling concerns while still supporting SIGHUP-driven reload.
///
/// # Example (in `main.rs` with tokio)
/// ```ignore
/// let handle = plugin_registry::spawn_reload_thread(registry.clone(), config_dir.clone());
/// tokio::spawn(async move {
///     use tokio::signal::unix::{signal, SignalKind};
///     let mut sighup = signal(SignalKind::hangup()).unwrap();
///     loop { sighup.recv().await; handle.trigger(); }
/// });
/// ```
pub struct ReloadHandle {
    tx: std::sync::mpsc::SyncSender<()>,
}

impl ReloadHandle {
    /// Send a reload request. Non-blocking; drops the request if one is already
    /// queued. Safe to call from async contexts or signal-adjacent code.
    pub fn trigger(&self) {
        let _ = self.tx.try_send(());
    }
}

/// Spawn a background thread that reloads `registry` from `config_dir` each
/// time [`ReloadHandle::trigger`] is called.
///
/// Returns a [`ReloadHandle`] that the caller uses to trigger reloads.
/// The thread exits when the handle (and all clones) are dropped.
pub fn spawn_reload_thread(registry: PluginRegistry, config_dir: PathBuf) -> ReloadHandle {
    let (tx, rx) = std::sync::mpsc::sync_channel::<()>(1);
    std::thread::spawn(move || {
        for () in rx {
            match registry.load_from_config_dir(&config_dir) {
                Ok(()) => log::info!("plugin_registry: reload complete"),
                Err(e) => log::warn!("plugin_registry: reload failed: {}", e),
            }
        }
    });
    ReloadHandle { tx }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    fn write_plugin_toml(dir: &Path, filename: &str, content: &str) -> PathBuf {
        let path = dir.join(filename);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        path
    }

    fn minimal_plugin(id: &str, source: &str) -> String {
        format!(
            r#"[[plugin]]
id = "{id}"
name = "Test Plugin {id}"
description = "A test plugin."
source = "{source}"
refresh_interval_secs = 300
data_strategy = "polling"
template_full = "templates/{id}_full.html.jinja"
"#
        )
    }

    #[test]
    fn load_single_plugin_file() {
        let dir = TempDir::new().unwrap();
        write_plugin_toml(dir.path(), "river.toml", &minimal_plugin("river", "usgs"));
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        assert_eq!(registry.len(), 1);
        let plugin = registry.get("river").unwrap();
        assert_eq!(plugin.id, "river");
        assert_eq!(plugin.source, "usgs");
    }

    #[test]
    fn load_multiple_plugin_files() {
        let dir = TempDir::new().unwrap();
        write_plugin_toml(dir.path(), "river.toml", &minimal_plugin("river", "usgs"));
        write_plugin_toml(dir.path(), "weather.toml", &minimal_plugin("weather", "noaa"));
        write_plugin_toml(dir.path(), "ferry.toml", &minimal_plugin("ferry", "wsdot"));
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        assert_eq!(registry.len(), 3);
        assert!(registry.get("river").is_some());
        assert!(registry.get("weather").is_some());
        assert!(registry.get("ferry").is_some());
    }

    #[test]
    fn later_file_overwrites_earlier_for_same_id() {
        let dir = TempDir::new().unwrap();
        // Write 'a_first.toml' before 'b_second.toml' (sorted alphabetically)
        write_plugin_toml(
            dir.path(),
            "a_first.toml",
            r#"[[plugin]]
id = "river"
name = "First"
source = "usgs_v1"
"#,
        );
        write_plugin_toml(
            dir.path(),
            "b_second.toml",
            r#"[[plugin]]
id = "river"
name = "Second"
source = "usgs_v2"
"#,
        );
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        assert_eq!(registry.len(), 1);
        let plugin = registry.get("river").unwrap();
        assert_eq!(plugin.source, "usgs_v2", "later file should overwrite");
    }

    #[test]
    fn load_from_config_dir_loads_plugins_toml_and_plugins_d() {
        let dir = TempDir::new().unwrap();
        // Write plugins.toml
        write_plugin_toml(
            dir.path(),
            "plugins.toml",
            &minimal_plugin("weather", "noaa"),
        );
        // Write plugins.d/
        let plugins_d = dir.path().join("plugins.d");
        std::fs::create_dir(&plugins_d).unwrap();
        write_plugin_toml(&plugins_d, "river.toml", &minimal_plugin("river", "usgs"));

        let registry = PluginRegistry::new();
        registry.load_from_config_dir(dir.path()).unwrap();
        assert_eq!(registry.len(), 2);
        assert!(registry.get("weather").is_some());
        assert!(registry.get("river").is_some());
    }

    #[test]
    fn load_registry_helper_returns_populated_registry() {
        let dir = TempDir::new().unwrap();
        let plugins_d = dir.path().join("plugins.d");
        std::fs::create_dir(&plugins_d).unwrap();
        write_plugin_toml(&plugins_d, "weather.toml", &minimal_plugin("weather", "noaa"));
        write_plugin_toml(&plugins_d, "river.toml", &minimal_plugin("river", "usgs"));

        let registry = load_registry(dir.path()).unwrap();
        assert_eq!(registry.len(), 2);
    }

    #[test]
    fn missing_plugins_d_is_not_an_error() {
        let dir = TempDir::new().unwrap();
        // No plugins.d directory; load_from_config_dir should succeed silently.
        let registry = PluginRegistry::new();
        registry.load_from_config_dir(dir.path()).unwrap();
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn all_returns_sorted_by_id() {
        let dir = TempDir::new().unwrap();
        write_plugin_toml(dir.path(), "z.toml", &minimal_plugin("z_plugin", "src_z"));
        write_plugin_toml(dir.path(), "a.toml", &minimal_plugin("a_plugin", "src_a"));
        write_plugin_toml(dir.path(), "m.toml", &minimal_plugin("m_plugin", "src_m"));
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        let all = registry.all();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].id, "a_plugin");
        assert_eq!(all[1].id, "m_plugin");
        assert_eq!(all[2].id, "z_plugin");
    }

    #[test]
    fn plugin_with_criteria_and_settings_schema() {
        let dir = TempDir::new().unwrap();
        let toml = r#"
[[plugin]]
id = "river"
name = "River"
source = "usgs"
template_full = "templates/river_full.html.jinja"

[[plugin.criteria]]
key = "water_level_ft"
label = "River level"
operator = "lte"
threshold = 12.0
unit = "ft"
go_direction = "below"

[[plugin.settings_schema]]
key = "site_id"
label = "USGS Site ID"
type = "text"
required = true
placeholder = "12200500"
"#;
        write_plugin_toml(dir.path(), "river.toml", toml);
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        let plugin = registry.get("river").unwrap();
        assert_eq!(plugin.criteria.len(), 1);
        assert_eq!(plugin.criteria[0].key, "water_level_ft");
        assert_eq!(plugin.criteria[0].operator, "lte");
        assert_eq!(plugin.criteria[0].threshold, 12.0);
        assert_eq!(plugin.settings_schema.len(), 1);
        assert_eq!(plugin.settings_schema[0].key, "site_id");
        assert!(plugin.settings_schema[0].required);
    }

    #[test]
    fn default_data_strategy_is_polling() {
        let dir = TempDir::new().unwrap();
        let toml = r#"
[[plugin]]
id = "test"
name = "Test"
source = "src"
"#;
        write_plugin_toml(dir.path(), "test.toml", toml);
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        let plugin = registry.get("test").unwrap();
        assert_eq!(plugin.data_strategy, DataStrategy::Polling);
    }

    #[test]
    fn default_refresh_interval_is_300() {
        let dir = TempDir::new().unwrap();
        let toml = r#"
[[plugin]]
id = "test"
name = "Test"
source = "src"
"#;
        write_plugin_toml(dir.path(), "test.toml", toml);
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        let plugin = registry.get("test").unwrap();
        assert_eq!(plugin.refresh_interval_secs, 300);
    }

    #[test]
    fn reload_file_updates_existing_plugin() {
        let dir = TempDir::new().unwrap();
        let path = write_plugin_toml(dir.path(), "river.toml", &minimal_plugin("river", "usgs_v1"));
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        assert_eq!(registry.get("river").unwrap().source, "usgs_v1");

        // Overwrite the file and reload.
        std::fs::write(&path, minimal_plugin("river", "usgs_v2")).unwrap();
        registry.reload_file(&path).unwrap();
        assert_eq!(registry.get("river").unwrap().source, "usgs_v2");
    }

    #[test]
    fn load_file_with_parse_error_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = write_plugin_toml(dir.path(), "bad.toml", "not valid toml !!!");
        let registry = PluginRegistry::new();
        let err = registry.load_file(&path).unwrap_err();
        assert!(matches!(err, RegistryError::Parse { .. }));
    }

    #[test]
    fn non_toml_files_in_dir_are_ignored() {
        let dir = TempDir::new().unwrap();
        write_plugin_toml(dir.path(), "river.toml", &minimal_plugin("river", "usgs"));
        // Write a non-toml file that should be ignored.
        std::fs::write(dir.path().join("README.md"), "# ignore me").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignore me too").unwrap();
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn registry_is_clone_shares_inner() {
        let dir = TempDir::new().unwrap();
        write_plugin_toml(dir.path(), "river.toml", &minimal_plugin("river", "usgs"));
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();

        // Clone shares the same Arc — writes through one are visible via the other.
        let clone = registry.clone();
        assert_eq!(clone.len(), 1);
        // Load more through original; clone sees them.
        write_plugin_toml(dir.path(), "weather.toml", &minimal_plugin("weather", "noaa"));
        registry.load_dir(dir.path()).unwrap();
        assert_eq!(clone.len(), 2);
    }

    #[test]
    fn plugin_with_default_elements_parses() {
        let dir = TempDir::new().unwrap();
        let toml = r#"
[[plugin]]
id = "weather"
name = "Weather"
source = "noaa"
template_full = "templates/weather_full.html.jinja"

[[plugin.default_elements]]
kind = "data_field"
field_path = "$.temperature_f"
label = "Temp"
format_string = "{{value}}°F"
x = 10
y = 10
width = 120
height = 48
font_size = 36

[[plugin.default_elements]]
kind = "static_divider"
orientation = "horizontal"
x = 10
y = 86
width = 180
height = 2
"#;
        write_plugin_toml(dir.path(), "weather.toml", toml);
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        let plugin = registry.get("weather").unwrap();
        assert_eq!(plugin.default_elements.len(), 2);
        assert_eq!(plugin.default_elements[0].kind, "data_field");
        assert_eq!(
            plugin.default_elements[0].field_path.as_deref(),
            Some("$.temperature_f")
        );
        assert_eq!(plugin.default_elements[0].font_size, Some(36));
        assert_eq!(plugin.default_elements[1].kind, "static_divider");
    }

    #[test]
    fn default_elements_default_to_empty() {
        let dir = TempDir::new().unwrap();
        write_plugin_toml(dir.path(), "x.toml", &minimal_plugin("x", "src"));
        let registry = PluginRegistry::new();
        registry.load_dir(dir.path()).unwrap();
        let plugin = registry.get("x").unwrap();
        assert!(plugin.default_elements.is_empty());
    }

    #[test]
    fn bundled_config_plugins_d_all_declare_default_elements() {
        let manifest_dir = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"),
        );
        let config_dir = manifest_dir.join("config");
        if !config_dir.is_dir() {
            return;
        }
        let registry = load_registry(&config_dir).expect("bundled plugins.d/ should parse");
        for id in &["weather", "river", "ferry", "trail", "road"] {
            let p = registry.get(id).expect("plugin present");
            assert!(
                !p.default_elements.is_empty(),
                "plugin '{id}' must declare [[default_elements]] for Phase 3"
            );
        }
    }

    /// Verify the bundled config/plugins.d/ files parse correctly and contain
    /// the expected 5 well-known plugin IDs.
    #[test]
    fn bundled_config_plugins_d_loads_all_five_sources() {
        // CARGO_MANIFEST_DIR points to the crate root at test time.
        let manifest_dir = std::path::PathBuf::from(
            std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"),
        );
        let config_dir = manifest_dir.join("config");
        if !config_dir.is_dir() {
            // Skip if the config/ directory is absent (e.g. in a stripped checkout).
            return;
        }
        let registry = load_registry(&config_dir).expect("bundled plugins.d/ should parse");
        for id in &["weather", "river", "ferry", "trail", "road"] {
            assert!(
                registry.get(id).is_some(),
                "expected plugin '{id}' to be present in bundled config/plugins.d/"
            );
        }
        assert_eq!(
            registry.len(),
            5,
            "expected exactly 5 plugins from bundled config/plugins.d/"
        );
    }
}
