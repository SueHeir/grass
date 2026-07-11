//! TOML config loading for grass apps.
//!
//! Plugins are constructed in code (you know your physics before compile);
//! their *parameters* are seeded from a TOML file at startup. The pattern:
//!
//!   1. [`InputPlugin`] parses the CLI, reads the file at `args[1]`, and
//!      installs a [`Config`] resource holding the parsed `toml::Table`.
//!   2. Each plugin's `build()` calls [`Config::load::<MyConfig>(app, "my_section")`]
//!      for optional config, or
//!      [`Config::load_required::<MyConfig>(app, "my_section")`] for required
//!      config, to deserialize its `[my_section]` slice and register it as an
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
//! Non-optional plugins should instead use [`Config::required_section`] or
//! [`Config::load_required`] so a missing or misspelled TOML section is reported
//! as a config error naming the absent section.
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
use std::fmt;
use std::path::{Path, PathBuf};

use grass_app::{App, ConfigDescription, ConfigSnippets, GenerateConfigFlag, Plugin};
use serde::{Deserialize, Serialize};

// ─── Config resource ────────────────────────────────────────────────────────

/// An actionable config-read error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    /// The supplied path does not name a TOML file.
    InvalidFileExtension {
        /// Path supplied by the caller.
        path: String,
    },
    /// Reading the TOML file failed.
    ReadFile {
        /// File that could not be read.
        path: String,
        /// The operating-system error.
        message: String,
    },
    /// Parsing TOML text failed.
    ParseToml {
        /// Origin of the TOML text (a path, or `"<string>"`).
        path: String,
        /// The TOML parser error.
        message: String,
    },
    /// A required `[key]` section was absent from the parsed TOML table.
    MissingSection {
        /// The missing top-level section name, without brackets.
        key: String,
    },
    /// A required config read was attempted before an App had a [`Config`] resource.
    MissingConfigResource {
        /// The required top-level section name, without brackets.
        key: String,
    },
    /// A present `[key]` section failed to deserialize into the requested type.
    InvalidSection {
        /// The top-level section name, without brackets.
        key: String,
        /// The TOML deserialization error.
        message: String,
    },
    /// A present `[[key]]` value was not an array or one of its entries did
    /// not deserialize into the requested type.
    InvalidArray {
        /// The top-level array name, without brackets.
        key: String,
        /// The failing entry index, if the value was an array.
        index: Option<usize>,
        /// The TOML deserialization error or a shape description.
        message: String,
    },
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::InvalidFileExtension { path } => {
                write!(f, "input file must be a .toml file, got '{path}'")
            }
            ConfigError::ReadFile { path, message } => {
                write!(f, "failed to read '{path}': {message}")
            }
            ConfigError::ParseToml { path, message } => {
                write!(f, "failed to parse TOML '{path}': {message}")
            }
            ConfigError::MissingSection { key } => write!(
                f,
                "missing required [{}] section in config file. Add a [{}] section, \
                 or run with --generate-config to see the sections this app expects.",
                key, key
            ),
            ConfigError::MissingConfigResource { key } => write!(
                f,
                "cannot read required [{}] section because no Config resource is installed. \
                 Add InputPlugin before this plugin, or seed Config in tests.",
                key
            ),
            ConfigError::InvalidSection { key, message } => {
                write!(
                    f,
                    "failed to parse [{}] section in config file: {}",
                    key, message
                )
            }
            ConfigError::InvalidArray {
                key,
                index,
                message,
            } => match index {
                Some(index) => write!(
                    f,
                    "failed to parse [[{key}]] entry {index} in config file: {message}"
                ),
                None => write!(
                    f,
                    "expected [[{key}]] to be an array in config file: {message}"
                ),
            },
        }
    }
}

impl std::error::Error for ConfigError {}

/// Wraps a parsed TOML table. Plugins reach into it with [`Self::section`] /
/// [`Self::required_section`] / [`Self::load`] / [`Self::parse_array`] in
/// `Plugin::build`.
#[derive(Debug)]
pub struct Config {
    /// The parsed top-level TOML table backing this config.
    pub table: toml::Table,
}

impl Config {
    /// Construct from an already-parsed TOML table. Useful in tests:
    /// `app.add_resource(Config::from_table(my_table))`.
    pub fn from_table(table: toml::Table) -> Self {
        Self { table }
    }

    /// Construct from a TOML string. Panics on parse error — meant for
    /// tests with hardcoded TOML literals. Programmatic callers should use
    /// [`Self::try_from_str`].
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(toml_str: &str) -> Self {
        Self::try_from_str(toml_str).unwrap_or_else(|error| panic!("Config::from_str: {error}"))
    }

    /// Construct from TOML text without panicking.
    pub fn try_from_str(toml_str: &str) -> Result<Self, ConfigError> {
        toml::from_str(toml_str)
            .map(Self::from_table)
            .map_err(|error| ConfigError::ParseToml {
                path: "<string>".to_string(),
                message: error.to_string(),
            })
    }

    /// Deserialize an optional `[key]` section, returning `T::default()` if
    /// the section is absent. Panics if deserialization fails (typo / wrong
    /// type); programmatic callers should use [`Self::try_section`].
    ///
    /// Use this for opt-in plugins where omitting `[key]` means "use defaults"
    /// or "do not register this plugin". Use [`Self::required_section`] for
    /// plugins whose section must appear in the user's TOML.
    pub fn section<T: for<'de> Deserialize<'de> + Default>(&self, key: &str) -> T {
        self.try_section(key)
            .unwrap_or_else(|error| panic!("Config::section: {error}"))
    }

    /// Fallible form of [`Self::section`]. Missing optional sections still
    /// return `T::default()`; malformed present sections return an error.
    pub fn try_section<T: for<'de> Deserialize<'de> + Default>(
        &self,
        key: &str,
    ) -> Result<T, ConfigError> {
        match self.table.get(key) {
            None => Ok(T::default()),
            Some(value) => {
                value
                    .clone()
                    .try_into::<T>()
                    .map_err(|error| ConfigError::InvalidSection {
                        key: key.to_string(),
                        message: error.to_string(),
                    })
            }
        }
    }

    /// Deserialize a required `[key]` section.
    ///
    /// Missing sections are reported as config errors naming the absent
    /// section. Use this for non-optional plugins where silently falling back
    /// to defaults would hide a misspelled or omitted TOML section.
    pub fn required_section<T: for<'de> Deserialize<'de>>(&self, key: &str) -> T {
        self.try_required_section(key)
            .unwrap_or_else(|error| panic!("Config::required_section: {error}"))
    }

    /// Fallible form of [`Self::required_section`], useful for tests and
    /// callers that want to surface config errors without exiting.
    pub fn try_required_section<T: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
    ) -> Result<T, ConfigError> {
        match self.table.get(key) {
            None => Err(ConfigError::MissingSection {
                key: key.to_string(),
            }),
            Some(v) => v
                .clone()
                .try_into::<T>()
                .map_err(|e| ConfigError::InvalidSection {
                    key: key.to_string(),
                    message: e.to_string(),
                }),
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
        Self::try_load(app, key).unwrap_or_else(|error| panic!("Config::load: {error}"))
    }

    /// Loads a section through its typed, declarative description. This keeps
    /// the TOML key used for parsing and the key used for generated examples
    /// coupled to the same Rust config type.
    pub fn load_described<T: DescribedConfig + Clone + 'static>(app: &mut App) -> T {
        Self::load(app, &T::description().section)
    }

    /// Fallible form of [`Self::load`]. It preserves the optional-section
    /// behavior: if no `Config` resource or section is present, `T::default()`
    /// is registered and returned.
    pub fn try_load<T: for<'de> Deserialize<'de> + Default + Clone + 'static>(
        app: &mut App,
        key: &str,
    ) -> Result<T, ConfigError> {
        let value: T = if let Some(cell) = app.get_mut_resource(TypeId::of::<Config>()) {
            let raw = cell.borrow();
            let cfg = raw
                .downcast_ref::<Config>()
                .expect("Config resource has wrong type — this is a bug in grass_app");
            cfg.try_section::<T>(key)?
        } else {
            T::default()
        };
        app.add_resource(value.clone());
        Ok(value)
    }

    /// Extract a required `[key]` section, register the resulting `T` as an
    /// App resource, and return it.
    ///
    /// Unlike [`Self::load`], this reports a missing section as an error instead
    /// of registering `T::default()`. Use it for non-optional plugins whose
    /// config must be present in the input TOML.
    pub fn load_required<T: for<'de> Deserialize<'de> + Clone + 'static>(
        app: &mut App,
        key: &str,
    ) -> T {
        Self::try_load_required(app, key)
            .unwrap_or_else(|error| panic!("Config::load_required: {error}"))
    }

    /// Fallible form of [`Self::load_required`].
    pub fn try_load_required<T: for<'de> Deserialize<'de> + Clone + 'static>(
        app: &mut App,
        key: &str,
    ) -> Result<T, ConfigError> {
        let value: T = if let Some(cell) = app.get_mut_resource(TypeId::of::<Config>()) {
            let raw = cell.borrow();
            let cfg = raw
                .downcast_ref::<Config>()
                .expect("Config resource has wrong type — this is a bug in grass_app");
            cfg.try_required_section::<T>(key)?
        } else {
            return Err(ConfigError::MissingConfigResource {
                key: key.to_string(),
            });
        };
        app.add_resource(value.clone());
        Ok(value)
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
        self.try_for_subapp(name, base_dir)
            .unwrap_or_else(|error| panic!("Config::for_subapp: {error}"))
    }

    /// Fallible form of [`Self::for_subapp`]. In particular, errors reading a
    /// referenced `config_path` are returned to the programmatic caller.
    pub fn try_for_subapp(&self, name: &str, base_dir: Option<&Path>) -> Result<Self, ConfigError> {
        let mut base = match self.subapp_config_path(name) {
            Some(path) => {
                let resolved = match base_dir {
                    Some(dir) if Path::new(&path).is_relative() => dir.join(&path),
                    _ => PathBuf::from(&path),
                };
                try_load_toml(&resolved.to_string_lossy())?
            }
            None => toml::Table::new(),
        };

        if let Some(toml::Value::Table(overrides)) = self.table.get(name) {
            deep_merge(&mut base, overrides);
        }

        Ok(Config { table: base })
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
    /// `Vec` if the key is missing. Panics on malformed input; programmatic
    /// callers should use [`Self::try_parse_array`].
    ///
    /// Use for fix-style entries where multiple instances of the same
    /// "kind" of plugin share a TOML key (`[[addforce]]`, `[[wall]]`, …).
    pub fn parse_array<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Vec<T> {
        self.try_parse_array(key)
            .unwrap_or_else(|error| panic!("Config::parse_array: {error}"))
    }

    /// Fallible form of [`Self::parse_array`].
    pub fn try_parse_array<T: for<'de> Deserialize<'de>>(
        &self,
        key: &str,
    ) -> Result<Vec<T>, ConfigError> {
        match self.table.get(key) {
            Some(toml::Value::Array(arr)) => arr
                .iter()
                .enumerate()
                .map(|(index, value)| {
                    value
                        .clone()
                        .try_into::<T>()
                        .map_err(|error| ConfigError::InvalidArray {
                            key: key.to_string(),
                            index: Some(index),
                            message: error.to_string(),
                        })
                })
                .collect(),
            Some(_) => Err(ConfigError::InvalidArray {
                key: key.to_string(),
                index: None,
                message: "value is not an array".to_string(),
            }),
            None => Ok(Vec::new()),
        }
    }
}

/// A Serde-compatible config type that publishes declarative metadata for the
/// same section it parses. The metadata powers generated TOML examples and
/// field-reference comments; Serde remains the source of parsing semantics.
///
/// It deliberately requires [`Serialize`] as well as [`Deserialize`]: the
/// `ConfigDescription` derive serializes defaults into TOML, and therefore
/// cannot truthfully describe a deserialize-only type.
pub trait DescribedConfig: for<'de> Deserialize<'de> + Serialize + Default {
    /// The section and fields represented by this parsing type.
    fn description() -> ConfigDescription;
}

/// Symbolic TOML values accepted by a configuration field.
///
/// `#[derive(ConfigDescription)]` implements this for configuration structs
/// (with no symbolic choices) and enums (using their declared variants).
/// Primitive and container implementations have no symbolic choices.
pub trait ConfigChoices {
    /// Declared symbolic values, in source order.
    fn choices() -> Vec<String>;
}

macro_rules! no_config_choices {
    ($($type:ty),* $(,)?) => {$(
        impl ConfigChoices for $type {
            fn choices() -> Vec<String> { Vec::new() }
        }
    )*};
}

no_config_choices!(
    bool,
    String,
    toml::Value,
    toml::Table,
    u8,
    u16,
    u32,
    u64,
    usize,
    i8,
    i16,
    i32,
    i64,
    isize,
    f32,
    f64,
);

impl<T: ConfigChoices> ConfigChoices for Option<T> {
    fn choices() -> Vec<String> {
        T::choices()
    }
}

impl<T> ConfigChoices for Vec<T> {
    fn choices() -> Vec<String> {
        Vec::new()
    }
}

#[cfg(test)]
mod described_config_tests {
    use super::*;
    use crate::{
        ClockConfig, DumpConfig, DumpPlugin, RunPlugin, SimClockPlugin, StageConfig, TermOutConfig,
        TermOutPlugin,
    };
    use grass_app::ConfigSnippets;
    use grass_derive::ConfigDescription as DeriveConfigDescription;

    #[derive(Clone, Default, Deserialize, serde::Serialize, DeriveConfigDescription)]
    enum ProbeMode {
        #[default]
        #[serde(rename = "fast")]
        Fast,
        Accurate,
    }

    #[derive(Clone, Default, Deserialize, serde::Serialize, DeriveConfigDescription)]
    #[config_description(section = "probe")]
    struct ProbeConfig {
        #[serde(default)]
        mode: ProbeMode,
    }

    fn default_from_generated<T: DescribedConfig>() -> T {
        let description = T::description();
        let config = Config::from_str(&description.render_toml());
        if description.array_table {
            config
                .table
                .get(&description.section)
                .expect("generated array table")
                .as_array()
                .expect("array table")
                .first()
                .expect("sample entry")
                .clone()
                .try_into()
                .expect("generated defaults deserialize")
        } else {
            config.section(&description.section)
        }
    }

    /// The default serialized by the parsed Rust type is the independent
    /// oracle here. A new typed field, a changed Rust default, or a removed
    /// metadata field makes this comparison fail instead of quietly relying on
    /// Serde's missing-field fallback.
    fn assert_description_matches_typed_default<T: DescribedConfig + serde::Serialize>() {
        let description = T::description();
        let typed: toml::Table = toml::from_str(
            &toml::to_string(&T::default()).expect("typed default serializes to TOML"),
        )
        .expect("serialized typed config is a TOML table");

        let generated = Config::from_str(&description.render_toml());
        let generated = if description.array_table {
            generated.table[&description.section]
                .as_array()
                .expect("generated array table")[0]
                .as_table()
                .expect("generated sample table")
        } else {
            generated.table[&description.section]
                .as_table()
                .expect("generated table")
        };

        let described: std::collections::BTreeSet<_> = description
            .fields
            .iter()
            .map(|field| field.name.as_str())
            .collect();
        let typed_fields: std::collections::BTreeSet<_> =
            typed.keys().map(String::as_str).collect();
        // `Option::None` is absent from the serialized typed value, but it is
        // still an advertised optional parser field. Every serialized field
        // must have an emitted example; metadata may additionally describe
        // such unset optional fields.
        assert!(typed_fields.is_subset(&described));

        for (name, value) in &typed {
            assert_eq!(generated.get(name), Some(value), "default drift for {name}");
            assert!(description
                .fields
                .iter()
                .any(|field| field.name == *name && field.example.is_some()));
        }
    }

    #[test]
    fn generated_defaults_match_typed_defaults() {
        assert_description_matches_typed_default::<ClockConfig>();
        assert_description_matches_typed_default::<DumpConfig>();
        assert_description_matches_typed_default::<TermOutConfig>();
        assert_description_matches_typed_default::<StageConfig>();
        assert_eq!(
            default_from_generated::<ClockConfig>().start_step,
            ClockConfig::default().start_step
        );
        assert_eq!(
            default_from_generated::<ClockConfig>().start_time,
            ClockConfig::default().start_time
        );
        assert_eq!(
            default_from_generated::<DumpConfig>().interval,
            DumpConfig::default().interval
        );
        assert_eq!(
            default_from_generated::<DumpConfig>().path_template,
            DumpConfig::default().path_template
        );
        assert_eq!(
            default_from_generated::<TermOutConfig>().every,
            TermOutConfig::default().every
        );
        assert_eq!(
            default_from_generated::<TermOutConfig>().columns,
            TermOutConfig::default().columns
        );
        assert_eq!(
            default_from_generated::<TermOutConfig>().width,
            TermOutConfig::default().width
        );
        assert_eq!(
            default_from_generated::<StageConfig>().steps,
            StageConfig::default().steps
        );
        assert!(default_from_generated::<StageConfig>().name.is_none());
    }

    #[test]
    fn generated_reference_includes_status_and_source_locations() {
        let text = TermOutConfig::description().render_toml();
        assert!(text.contains("Optional; default: 100."));
        assert!(text.contains("Source: crates/grass_io/src/term_out.rs:"));
        assert!(text.contains("TermOutConfig.every"));
    }

    /// This is deliberately stricter than round-tripping defaults: it checks
    /// the complete user-visible field contract (names, TOML types,
    /// requiredness, defaults, docs, and per-field source locations). A
    /// renamed, added, removed, or retyped Serde field changes the derive
    /// output and fails this test rather than leaving a parallel descriptor
    /// quietly stale.
    #[test]
    fn generated_contract_covers_every_builtin_field() {
        fn check<T: DescribedConfig>(expected: &[(&str, &str, bool, Option<&str>)]) {
            let description = T::description();
            assert_eq!(description.fields.len(), expected.len());
            for (field, (name, ty, required, default)) in
                description.fields.iter().zip(expected.iter().copied())
            {
                assert_eq!(field.name, name);
                assert_eq!(field.ty, ty);
                assert_eq!(field.required, required);
                assert_eq!(field.default.as_deref(), default);
                assert!(!field.description.is_empty());
                assert!(field.source.contains(':'));
                assert!(field.source.ends_with(&format!(".{}", name)));
            }
        }
        check::<ClockConfig>(&[
            ("start_step", "integer", false, Some("0")),
            ("start_time", "float", false, Some("0.0")),
        ]);
        check::<DumpConfig>(&[
            ("interval", "integer", false, Some("0")),
            (
                "path_template",
                "string",
                false,
                Some("\"frame_{step:06}.bin\""),
            ),
        ]);
        check::<TermOutConfig>(&[
            ("every", "integer", false, Some("100")),
            ("columns", "array", false, Some("[\"step\", \"time\"]")),
            ("width", "integer", false, Some("14")),
        ]);
        check::<StageConfig>(&[
            ("name", "optional", false, None),
            ("steps", "integer", false, Some("1000")),
            ("dt", "float", false, Some("0.0")),
            ("skip", "boolean", false, Some("false")),
            ("save_at_end", "boolean", false, Some("false")),
        ]);
    }

    #[test]
    fn enum_choices_come_from_the_serde_enum_definition() {
        let field = &ProbeConfig::description().fields[0];
        assert_eq!(field.name, "mode");
        assert_eq!(field.choices, ["fast", "Accurate"]);
    }

    #[test]
    fn built_in_plugins_collect_typed_examples() {
        let mut app = App::new();
        app.add_plugins(SimClockPlugin);
        app.add_plugins(TermOutPlugin);
        app.add_plugins(DumpPlugin::default());
        app.add_plugins(RunPlugin);
        let snippets = app
            .get_resource_ref::<ConfigSnippets>()
            .expect("built-in descriptions collected");
        assert!(snippets
            .snippets
            .iter()
            .any(|text| text.contains("[clock]")));
        assert!(snippets
            .snippets
            .iter()
            .any(|text| text.contains("Source: crates/grass_io/src/run.rs:")
                && text.contains("StageConfig.steps")));
    }

    #[test]
    fn built_in_unknown_fields_name_the_bad_key() {
        let config = Config::from_str("[clock]\nstart_stpe = 1\n");
        let error = config.try_section::<ClockConfig>("clock").unwrap_err();
        assert!(error.to_string().contains("unknown field `start_stpe`"));
    }
}

// ─── CLI input ──────────────────────────────────────────────────────────────

/// CLI metadata: the input filename and (optionally) an output directory
/// resolved from `[output] dir = "..."` in the TOML or the input file's
/// parent directory. Plugins that need to write output files can
/// `Res<Input>` to discover where.
pub struct Input {
    /// Path to the input TOML file (typically `args[1]`).
    pub filename: String,
    /// Resolved output directory, if configured or inferred; `None` otherwise.
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
        let table =
            try_load_toml(&input_file).unwrap_or_else(|error| report_cli_config_error(&error));

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
    /// Fallibly adds a named sub-App whose `Config` is pre-seeded from this
    /// App's `[<name>.*]` slice (and optional `config_path`) before `build`
    /// runs.
    ///
    /// Returns [`ConfigError`] when the referenced config file cannot be read
    /// or parsed. The build closure is not called when constructing that
    /// slice fails, so programmatic callers can report a startup diagnostic
    /// without panicking.
    fn try_add_subapp_with_config<F: FnOnce(&mut App)>(
        &mut self,
        name: &str,
        build: F,
    ) -> Result<&mut Self, ConfigError>;

    /// Adds a named sub-App whose `Config` is pre-seeded from this App's
    /// `[<name>.*]` slice (and optional `config_path`) before `build` runs.
    ///
    /// This compatibility convenience wrapper panics if a referenced config
    /// file cannot be read or parsed. Programmatic callers should use
    /// [`Self::try_add_subapp_with_config`].
    fn add_subapp_with_config<F: FnOnce(&mut App)>(&mut self, name: &str, build: F) -> &mut Self;
}

impl MultiIoExt for App {
    fn try_add_subapp_with_config<F: FnOnce(&mut App)>(
        &mut self,
        name: &str,
        build: F,
    ) -> Result<&mut Self, ConfigError> {
        let input_dir = self
            .get_resource_ref::<Input>()
            .and_then(|i| Path::new(&i.filename).parent().map(|p| p.to_path_buf()));
        let main_cfg = self
            .get_resource_ref::<Config>()
            .map(|c| Config::from_table(c.table.clone()))
            .unwrap_or_else(|| Config::from_str(""));
        let slice = main_cfg.try_for_subapp(name, input_dir.as_deref())?;

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

        // A generated parent config must be directly usable for the same
        // namespaced sub-app setup.  Re-home each child table beneath its
        // sub-app key while preserving all generated comments and values.
        let generated = sub
            .get_resource_ref::<ConfigSnippets>()
            .map(|snippets| snippets.snippets.clone())
            .unwrap_or_default()
            .into_iter()
            .map(|snippet| namespace_generated_table(name, snippet))
            .collect::<Vec<_>>();
        if !generated.is_empty() {
            if let Some(cell) = self.get_mut_resource(TypeId::of::<ConfigSnippets>()) {
                cell.borrow_mut()
                    .downcast_mut::<ConfigSnippets>()
                    .expect("ConfigSnippets resource has wrong type")
                    .snippets
                    .extend(generated);
            } else {
                self.add_resource(ConfigSnippets {
                    snippets: generated,
                });
            }
        }

        use grass_multi::MultiAppExt;
        self.add_subapp(name, sub);
        Ok(self)
    }

    fn add_subapp_with_config<F: FnOnce(&mut App)>(&mut self, name: &str, build: F) -> &mut Self {
        self.try_add_subapp_with_config(name, build)
            .unwrap_or_else(|error| panic!("App::add_subapp_with_config: {error}"))
    }
}

fn namespace_generated_table(namespace: &str, snippet: String) -> String {
    let mut namespaced = String::with_capacity(snippet.len() + namespace.len());
    let mut changed = false;
    for line in snippet.lines() {
        if !changed && (line.starts_with('[') && !line.starts_with("[[")) {
            let section = line.trim_start_matches('[').trim_end_matches(']');
            namespaced.push_str(&format!("[{namespace}.{section}]\n"));
            changed = true;
        } else if !changed && line.starts_with("[[") {
            let section = line.trim_start_matches("[[").trim_end_matches("]]");
            namespaced.push_str(&format!("[[{namespace}.{section}]]\n"));
            changed = true;
        } else {
            namespaced.push_str(line);
            namespaced.push('\n');
        }
    }
    namespaced
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

/// Read and parse a TOML file.
///
/// This compatibility convenience wrapper panics on failure. Library callers
/// that need to handle startup errors should use [`try_load_toml`].
pub fn load_toml(path: &str) -> toml::Table {
    try_load_toml(path).unwrap_or_else(|error| panic!("load_toml: {error}"))
}

/// Read and parse a TOML file without printing or terminating the process.
pub fn try_load_toml(path: &str) -> Result<toml::Table, ConfigError> {
    if !path.ends_with(".toml") {
        return Err(ConfigError::InvalidFileExtension {
            path: path.to_string(),
        });
    }
    let content = std::fs::read_to_string(path).map_err(|error| ConfigError::ReadFile {
        path: path.to_string(),
        message: error.to_string(),
    })?;
    toml::from_str(&content).map_err(|error| ConfigError::ParseToml {
        path: path.to_string(),
        message: error.to_string(),
    })
}

/// Render an input error at the executable boundary, then select the CLI exit
/// code. This is deliberately private to [`InputPlugin`]; reusable config APIs
/// return [`ConfigError`] instead.
fn report_cli_config_error(error: &ConfigError) -> ! {
    eprintln!("Error: {error}");
    std::process::exit(1);
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
    fn try_from_str_reports_malformed_toml() {
        let err = Config::try_from_str("[knobs\nsteps = 1")
            .expect_err("malformed TOML must be returned to programmatic callers");

        assert!(matches!(err, ConfigError::ParseToml { path, .. } if path == "<string>"));
    }

    #[test]
    fn try_section_reports_malformed_section() {
        let cfg = Config::from_str("[knobs]\nsteps = \"not an integer\"");
        let err = cfg
            .try_section::<Knobs>("knobs")
            .expect_err("wrong section field type must be returned");

        assert!(matches!(err, ConfigError::InvalidSection { key, .. } if key == "knobs"));
    }

    #[test]
    fn required_section_reads_typed_struct() {
        let cfg = Config::from_str(
            r#"
            [knobs]
            steps = 200
            dt = 1.0e-3
            "#,
        );
        let k: Knobs = cfg.required_section("knobs");
        assert_eq!(k.steps, 200);
        assert_eq!(k.dt, 1.0e-3);
    }

    #[test]
    fn try_required_section_reports_missing_section() {
        let cfg = Config::from_str("");
        let err = cfg
            .try_required_section::<Knobs>("knobs")
            .expect_err("missing required section should be an error");

        assert_eq!(
            err,
            ConfigError::MissingSection {
                key: "knobs".to_string()
            }
        );
        let message = err.to_string();
        assert!(message.contains("missing required [knobs] section"));
        assert!(message.contains("--generate-config"));
    }

    #[test]
    fn load_required_registers_resource_and_returns_value() {
        let mut app = App::new();
        app.add_resource(Config::from_str(
            r#"
            [knobs]
            steps = 7
            dt = 0.25
            "#,
        ));

        let k: Knobs = Config::load_required(&mut app, "knobs");
        assert_eq!(k.steps, 7);
        assert_eq!(k.dt, 0.25);

        let read = app.get_resource_ref::<Knobs>().expect("Knobs registered");
        assert_eq!(read.steps, 7);
        assert_eq!(read.dt, 0.25);
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
    fn try_parse_array_reports_malformed_entry() {
        let cfg = Config::from_str("[[force]]\ngx = \"not a float\"");
        let err = cfg
            .try_parse_array::<Force>("force")
            .expect_err("wrong array entry field type must be returned");

        assert!(matches!(
            err,
            ConfigError::InvalidArray {
                key,
                index: Some(0),
                ..
            } if key == "force"
        ));
    }

    #[test]
    fn try_load_toml_reports_missing_file() {
        let path = std::env::temp_dir().join("grass_io_config_that_does_not_exist.toml");
        let err = try_load_toml(&path.to_string_lossy())
            .expect_err("a missing config file must be returned as an error");

        assert!(
            matches!(err, ConfigError::ReadFile { path: error_path, .. } if error_path == path.to_string_lossy())
        );
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
    fn try_add_subapp_with_config_reports_missing_referenced_file() {
        let path = std::env::temp_dir().join("grass_io_missing_subapp_config.toml");
        let mut parent = App::new();
        parent.add_resource(Config::from_str(&format!(
            "[subapps.child]\nconfig_path = \"{}\"\n",
            path.display()
        )));

        let err = match parent.try_add_subapp_with_config("child", |_| {
            panic!("build must not run after config loading fails")
        }) {
            Ok(_) => panic!("missing referenced config must be returned as an error"),
            Err(err) => err,
        };

        assert!(
            matches!(err, ConfigError::ReadFile { path: error_path, .. } if error_path == path.to_string_lossy())
        );
    }

    #[test]
    fn try_add_subapp_with_config_reports_malformed_referenced_file() {
        let dir = std::env::temp_dir().join("grass_io_malformed_subapp_config");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("child.toml");
        std::fs::write(&path, "[knobs\nsteps = 1").unwrap();

        let mut parent = App::new();
        parent.add_resource(Config::from_str(&format!(
            "[subapps.child]\nconfig_path = \"{}\"\n",
            path.display()
        )));

        let err = match parent.try_add_subapp_with_config("child", |_| {
            panic!("build must not run after config loading fails")
        }) {
            Ok(_) => panic!("malformed referenced config must be returned as an error"),
            Err(err) => err,
        };

        assert!(
            matches!(err, ConfigError::ParseToml { path: error_path, .. } if error_path == path.to_string_lossy())
        );
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
