//! TOML config loading for grass apps.
//!
//! Plugins are constructed in code (you know your physics before compile);
//! their *parameters* are seeded from a TOML file at startup. The pattern:
//!
//!   1. [`InputPlugin`] parses the CLI, reads the file at `args[1]`, and
//!      installs a [`Config`] resource holding the parsed `toml::Table`.
//!   2. Each plugin's `build()` calls [`Config::load::<MyConfig>(app, "my_section")`]
//!      to deserialize its `[my_section]` slice and register it as an
//!      `Res<MyConfig>` for that plugin's systems to read.
//!   3. Plugins also call [`grass_app::App::add_config_snippet`] so
//!      `--generate-config` can dump a complete starter file.
//!
//! ## Conditional registration
//!
//! When a plugin should only register systems if the user opted in via
//! TOML (e.g. an DIRT-style `[gravity]` body force), the plugin checks
//! whether its config section exists or has non-default values, and
//! short-circuits its `build()` if not. The config-reading API supports
//! this by returning `T::default()` for missing sections.
//!
//! ## CLI surface
//!
//! `myapp <config.toml> [--generate-config]`
//!
//!   - `<config.toml>` — path to the input file.
//!   - `--generate-config` — print all registered plugins' config snippets
//!     and exit before running. The input path can be omitted in this mode.
//!
//! ## Programmatic use (tests)
//!
//! [`InputPlugin`] short-circuits if a [`Config`] resource is already
//! registered, so tests can `app.add_resource(Config { table: ... })` and
//! then `app.add_plugins(InputPlugin)` without the plugin clobbering the
//! seeded config.

use std::any::TypeId;
use std::env;
use std::path::{Path, PathBuf};

use grass_app::{App, GenerateConfigFlag, Plugin};
use serde::Deserialize;

// ─── Config resource ────────────────────────────────────────────────────────

/// Wraps a parsed TOML table. Plugins reach into it with [`Self::section`] /
/// [`Self::load`] / [`Self::parse_array`] in `Plugin::build`.
pub struct Config {
    pub table: toml::Table,
}

impl Config {
    /// Construct from an already-parsed TOML table. Useful in tests:
    /// `app.add_resource(Config::from_table(my_table))`.
    pub fn from_table(table: toml::Table) -> Self {
        Self { table }
    }

    /// Construct from a TOML string. Panics on parse error — meant for
    /// tests with hardcoded TOML literals.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_str: &str) -> Self {
        let table: toml::Table = toml::from_str(toml_str)
            .unwrap_or_else(|e| panic!("Config::from_str: TOML parse error: {e}"));
        Self { table }
    }

    /// Deserialize a `[key]` section, returning `T::default()` if the
    /// section is absent. Prints an actionable error and exits if
    /// deserialization fails (typo / wrong type).
    pub fn section<T: for<'de> Deserialize<'de> + Default>(&self, key: &str) -> T {
        match self.table.get(key) {
            None => T::default(),
            Some(v) => match v.clone().try_into::<T>() {
                Ok(val) => val,
                Err(e) => {
                    eprintln!();
                    eprintln!("ERROR: Failed to parse [{}] section in config file.", key);
                    eprintln!("  {}", e);
                    eprintln!();
                    eprintln!(
                        "  Hint: check that all field names are spelled correctly \
                         and values have the right type."
                    );
                    eprintln!(
                        "  Run with --generate-config to see a complete example \
                         configuration."
                    );
                    std::process::exit(1);
                }
            },
        }
    }

    /// Extract a `[key]` section, register the resulting `T` as an App
    /// resource, and return it. The standard "configure-after-construct"
    /// hook: a plugin's `build` calls this to seed its own config from
    /// the user's TOML.
    ///
    /// If no [`Config`] resource is on `app` (e.g. the plugin was added
    /// before [`InputPlugin`]), `T::default()` is registered.
    pub fn load<T: for<'de> Deserialize<'de> + Default + Clone + 'static>(
        app: &mut App,
        key: &str,
    ) -> T {
        let value: T = if let Some(cell) = app.get_mut_resource(TypeId::of::<Config>()) {
            let raw = cell.borrow();
            let cfg = raw
                .downcast_ref::<Config>()
                .expect("Config resource has wrong type — this is a bug in grass_app");
            cfg.section::<T>(key)
        } else {
            T::default()
        };
        app.add_resource(value.clone());
        value
    }

    /// Build a per-sub-App `Config` from this (parent) Config.
    ///
    /// Two compositional models, optionally combined:
    ///
    ///   - **Namespace prefix.** `[<name>.section]` keys in the parent
    ///     become `[section]` in the returned Config. `[a.oscillator]
    ///     dt = 0.001` in main.toml shows up as `[oscillator] dt = 0.001`
    ///     to sub-App `a`'s plugins.
    ///   - **File reference.** `[subapps.<name>] config_path = "..."`
    ///     points to a separate TOML file. The file's contents become
    ///     the base for sub-App `<name>`'s Config. Relative paths
    ///     resolve against `base_dir` (typically the directory the
    ///     parent's main.toml was loaded from).
    ///
    /// When both are present, the file is the base and the inline
    /// `[<name>.*]` keys are deep-merged on top — useful for per-run
    /// overrides without editing the per-domain file.
    ///
    /// If neither is present, returns a `Config` with an empty table —
    /// the sub-App's plugins will all see `T::default()` from
    /// [`Self::section`] / [`Self::load`].
    pub fn for_subapp(&self, name: &str, base_dir: Option<&Path>) -> Self {
        let mut base = match self.subapp_config_path(name) {
            Some(path) => {
                let resolved = match base_dir {
                    Some(dir) if Path::new(&path).is_relative() => dir.join(&path),
                    _ => PathBuf::from(&path),
                };
                load_toml(&resolved.to_string_lossy())
            }
            None => toml::Table::new(),
        };

        if let Some(toml::Value::Table(overrides)) = self.table.get(name) {
            deep_merge(&mut base, overrides);
        }

        Config { table: base }
    }

    fn subapp_config_path(&self, name: &str) -> Option<String> {
        self.table
            .get("subapps")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get(name))
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("config_path"))
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    /// Parse a `[[key]]` TOML array into a `Vec<T>`. Returns an empty
    /// `Vec` if the key is missing. Prints an actionable error and exits
    /// on per-entry deserialization failure.
    ///
    /// Use for fix-style entries where multiple instances of the same
    /// "kind" of plugin share a TOML key (`[[addforce]]`, `[[wall]]`, …).
    pub fn parse_array<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Vec<T> {
        match self.table.get(key) {
            Some(toml::Value::Array(arr)) => arr
                .iter()
                .enumerate()
                .map(|(idx, v)| match v.clone().try_into::<T>() {
                    Ok(val) => val,
                    Err(e) => {
                        eprintln!();
                        eprintln!(
                            "ERROR: Failed to parse [[{}]] entry {} in config file.",
                            key, idx
                        );
                        eprintln!("  {}", e);
                        eprintln!();
                        eprintln!(
                            "  Hint: check that all field names are spelled \
                             correctly and values have the right type."
                        );
                        eprintln!(
                            "  Run with --generate-config to see a complete example \
                             configuration."
                        );
                        std::process::exit(1);
                    }
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

// ─── CLI input ──────────────────────────────────────────────────────────────

/// CLI metadata: the input filename and (optionally) an output directory
/// resolved from `[output] dir = "..."` in the TOML or the input file's
/// parent directory. Plugins that need to write output files can
/// `Res<Input>` to discover where.
pub struct Input {
    pub filename: String,
    pub output_dir: Option<String>,
}

// ─── InputPlugin ────────────────────────────────────────────────────────────

/// Parses CLI args, reads `args[1]` as a TOML file, and installs a
/// [`Config`] + [`Input`] on the App. Skip with `--generate-config` to
/// install an empty `Config` and a [`GenerateConfigFlag`] (so plugins
/// emit their snippets via `App::add_config_snippet`
/// and the App exits before running).
///
/// **CLI surface:** `myapp <config.toml> [--generate-config]`
///
/// **Programmatic use:** if a [`Config`] resource is already present on
/// the App when this plugin runs `build`, CLI parsing is skipped — tests
/// can seed `Config` with `Config::from_str(...)` and then add this
/// plugin without it clobbering the seeded value.
pub struct InputPlugin;

impl Plugin for InputPlugin {
    fn build(&self, app: &mut App) {
        // Programmatic seed wins.
        if app.get_resource_ref::<Config>().is_some() {
            return;
        }

        let args: Vec<String> = env::args().collect();

        if args.iter().any(|a| a == "--generate-config") {
            app.add_resource(Config {
                table: toml::Table::new(),
            });
            app.add_resource(Input {
                filename: String::new(),
                output_dir: None,
            });
            app.add_resource(GenerateConfigFlag);
            return;
        }

        let input_file = args.get(1).cloned().unwrap_or_else(|| {
            eprintln!("Usage: <binary> <input.toml> [--generate-config]");
            std::process::exit(1);
        });
        let table = load_toml(&input_file);

        // Output directory: prefer [output] dir from config, else the
        // input file's parent.
        let output_dir = table
            .get("output")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("dir"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                std::path::Path::new(&input_file)
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| p.to_string_lossy().into_owned())
            });

        app.add_resource(Input {
            filename: input_file,
            output_dir,
        });
        app.add_resource(Config { table });
    }
}

// ─── Sub-App registration with auto-sliced Config ──────────────────────────

/// Extension trait on [`App`] adding a one-call helper for registering
/// a sub-App pre-seeded with its [`Config`] slice from the parent's
/// main TOML.
///
/// `parent.add_subapp_with_config("dem", |app| { ... })` is equivalent
/// to:
///
/// ```rust,ignore
/// let input_dir = parent.get_resource_ref::<Input>()
///     .and_then(|i| Path::new(&i.filename).parent().map(|p| p.to_path_buf()));
/// let main_cfg = parent.get_resource_ref::<Config>()
///     .map(|c| Config::from_table(c.table.clone()))
///     .unwrap_or_else(|| Config::from_str(""));
/// let slice = main_cfg.for_subapp("dem", input_dir.as_deref());
/// let mut sub = App::new();
/// sub.add_resource(slice);
/// /* user closure registers plugins on `sub` */
/// parent.add_subapp("dem", sub);
/// ```
///
/// The closure receives the in-progress sub-App with its `Config`
/// already seeded from the `[dem.*]` slice (and optional `[subapps.dem]
/// config_path`). Anything the closure adds — plugins, resources,
/// systems — runs against that pre-seeded `Config`.
pub trait MultiIoExt {
    fn add_subapp_with_config<F: FnOnce(&mut App)>(&mut self, name: &str, build: F) -> &mut Self;
}

impl MultiIoExt for App {
    fn add_subapp_with_config<F: FnOnce(&mut App)>(&mut self, name: &str, build: F) -> &mut Self {
        let input_dir = self
            .get_resource_ref::<Input>()
            .and_then(|i| Path::new(&i.filename).parent().map(|p| p.to_path_buf()));
        let main_cfg = self
            .get_resource_ref::<Config>()
            .map(|c| Config::from_table(c.table.clone()))
            .unwrap_or_else(|| Config::from_str(""));
        let slice = main_cfg.for_subapp(name, input_dir.as_deref());

        // Seed `Input` on the sub-App so plugins that resolve relative
        // output paths (DIRT's print/dump systems, `grass_io::DumpPlugin`)
        // see the slice's `[output] dir`. Without this each example would
        // need a `seed_subapp_input(app)` helper that re-implemented this
        // lookup. The build closure can still overwrite the Input
        // resource if it wants something different.
        let sub_output_dir = slice
            .table
            .get("output")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("dir"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let mut sub = App::new();
        sub.add_resource(slice);
        sub.add_resource(Input {
            filename: String::new(),
            output_dir: sub_output_dir,
        });
        build(&mut sub);

        use grass_multi::MultiAppExt;
        self.add_subapp(name, sub);
        self
    }
}

/// Recursive merge — for each key in `overrides`, if both sides have a
/// table at that key, recurse; otherwise overwrite. Used by
/// [`Config::for_subapp`] to apply inline overrides on top of a
/// `config_path`-loaded base.
pub fn deep_merge(base: &mut toml::Table, overrides: &toml::Table) {
    for (key, override_val) in overrides {
        match (base.get_mut(key), override_val) {
            (Some(toml::Value::Table(base_tbl)), toml::Value::Table(override_tbl)) => {
                deep_merge(base_tbl, override_tbl);
            }
            _ => {
                base.insert(key.clone(), override_val.clone());
            }
        }
    }
}

/// Read and parse a TOML file. Prints an actionable error and exits on
/// missing / unreadable / malformed files.
pub fn load_toml(path: &str) -> toml::Table {
    if !path.ends_with(".toml") {
        eprintln!("Error: input file must be a .toml file, got '{}'", path);
        std::process::exit(1);
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| {
        eprintln!("Error reading '{}': {}", path, e);
        std::process::exit(1);
    });
    toml::from_str(&content).unwrap_or_else(|e| {
        eprintln!("Error parsing TOML '{}': {}", path, e);
        std::process::exit(1);
    })
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Clone, Deserialize)]
    struct Knobs {
        #[serde(default)]
        steps: u64,
        #[serde(default)]
        dt: f64,
    }

    #[derive(Debug, Default, Clone, Deserialize)]
    struct Force {
        #[serde(default)]
        gx: f64,
        #[serde(default)]
        gz: f64,
    }

    #[test]
    fn section_reads_typed_struct() {
        let cfg = Config::from_str(
            r#"
            [knobs]
            steps = 200
            dt = 1.0e-3
            "#,
        );
        let k: Knobs = cfg.section("knobs");
        assert_eq!(k.steps, 200);
        assert_eq!(k.dt, 1.0e-3);
    }

    #[test]
    fn section_returns_default_when_missing() {
        let cfg = Config::from_str("");
        let k: Knobs = cfg.section("knobs");
        assert_eq!(k.steps, 0);
        assert_eq!(k.dt, 0.0);
    }

    #[test]
    fn parse_array_handles_missing_key() {
        let cfg = Config::from_str("");
        let v: Vec<Force> = cfg.parse_array("force");
        assert!(v.is_empty());
    }

    #[test]
    fn parse_array_handles_array_of_tables() {
        let cfg = Config::from_str(
            r#"
            [[force]]
            gx = 1.0
            [[force]]
            gz = -9.81
            "#,
        );
        let v: Vec<Force> = cfg.parse_array("force");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].gx, 1.0);
        assert_eq!(v[1].gz, -9.81);
    }

    #[test]
    fn load_registers_resource_and_returns_value() {
        let mut app = App::new();
        app.add_resource(Config::from_str(
            r#"
            [knobs]
            steps = 42
            "#,
        ));
        let k: Knobs = Config::load(&mut app, "knobs");
        assert_eq!(k.steps, 42);

        // Resource is installed on the app:
        let read = app.get_resource_ref::<Knobs>().expect("Knobs registered");
        assert_eq!(read.steps, 42);
    }

    #[test]
    fn load_falls_back_to_default_when_no_config() {
        let mut app = App::new();
        let k: Knobs = Config::load(&mut app, "knobs");
        assert_eq!(k.steps, 0);
    }

    #[test]
    fn for_subapp_extracts_namespace_prefix() {
        let main = Config::from_str(
            r#"
            [clock]
            start_step = 0

            [a.knobs]
            steps = 100
            dt = 1e-3

            [b.knobs]
            steps = 200
            dt = 5e-4
            "#,
        );
        let a = main.for_subapp("a", None);
        let k_a: Knobs = a.section("knobs");
        assert_eq!(k_a.steps, 100);
        assert_eq!(k_a.dt, 1e-3);

        let b = main.for_subapp("b", None);
        let k_b: Knobs = b.section("knobs");
        assert_eq!(k_b.steps, 200);
        assert_eq!(k_b.dt, 5e-4);

        // Sibling [clock] shouldn't leak into a/b:
        assert!(a.table.get("clock").is_none());
    }

    #[test]
    fn for_subapp_with_no_section_returns_empty() {
        let main = Config::from_str("[clock]\nstart_step = 0\n");
        let a = main.for_subapp("missing", None);
        let k: Knobs = a.section("knobs");
        assert_eq!(k.steps, 0);
    }

    #[test]
    fn for_subapp_loads_from_file_path() {
        let dir = std::env::temp_dir().join("grass_io_for_subapp_file");
        std::fs::create_dir_all(&dir).unwrap();
        let dem_path = dir.join("dem.toml");
        std::fs::write(&dem_path, "[knobs]\nsteps = 999\ndt = 0.7\n").unwrap();

        let main = Config::from_str(&format!(
            r#"
            [subapps.dem]
            config_path = "{}"
            "#,
            dem_path.display()
        ));
        let dem = main.for_subapp("dem", None);
        let k: Knobs = dem.section("knobs");
        assert_eq!(k.steps, 999);
        assert_eq!(k.dt, 0.7);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn for_subapp_inline_overrides_file_via_deep_merge() {
        let dir = std::env::temp_dir().join("grass_io_for_subapp_merge");
        std::fs::create_dir_all(&dir).unwrap();
        let dem_path = dir.join("dem.toml");
        std::fs::write(&dem_path, "[knobs]\nsteps = 50\ndt = 1e-3\n").unwrap();

        let main = Config::from_str(&format!(
            r#"
            [subapps.dem]
            config_path = "{}"

            [dem.knobs]
            dt = 5e-4
            "#,
            dem_path.display()
        ));
        let dem = main.for_subapp("dem", None);
        let k: Knobs = dem.section("knobs");
        // `steps` from file, `dt` overridden inline:
        assert_eq!(k.steps, 50);
        assert_eq!(k.dt, 5e-4);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deep_merge_recurses_into_nested_tables() {
        let mut base: toml::Table =
            toml::from_str("[knobs]\nsteps = 100\ndt = 1e-3\n[other]\nz = 5\n").unwrap();
        let overrides: toml::Table =
            toml::from_str("[knobs]\ndt = 5e-4\n[extra]\nq = 7\n").unwrap();
        deep_merge(&mut base, &overrides);
        let knobs = base["knobs"].as_table().unwrap();
        assert_eq!(knobs["steps"].as_integer(), Some(100));
        assert_eq!(knobs["dt"].as_float(), Some(5e-4));
        assert!(base.contains_key("other"));
        assert!(base.contains_key("extra"));
    }
}
