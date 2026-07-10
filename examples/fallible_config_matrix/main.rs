//! Typed config/startup diagnostics without a process exit.
//!
//! Run with `cargo run --example fallible_config_matrix`.

use grass_app::App;
use grass_io::{Config, ConfigError, MultiIoExt};

const CASES: [&str; 4] = [
    "malformed_inline",
    "missing_section",
    "missing_referenced_file",
    "malformed_referenced_file",
];

fn expected(case: &str) -> String {
    let config = Config::try_from_str(include_str!("config.toml"))
        .expect("example's declarative config must be valid");
    config
        .table
        .get("checks")
        .and_then(toml::Value::as_table)
        .and_then(|checks| checks.get(case))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .expect("every matrix case needs an expected ConfigError variant")
}

fn variant(error: &ConfigError) -> &'static str {
    match error {
        ConfigError::InvalidFileExtension { .. } => "InvalidFileExtension",
        ConfigError::ReadFile { .. } => "ReadFile",
        ConfigError::ParseToml { .. } => "ParseToml",
        ConfigError::MissingSection { .. } => "MissingSection",
        ConfigError::MissingConfigResource { .. } => "MissingConfigResource",
        ConfigError::InvalidSection { .. } => "InvalidSection",
        ConfigError::InvalidArray { .. } => "InvalidArray",
    }
}

fn subapp_error(config_path: &str) -> ConfigError {
    let mut app = App::new();
    app.add_resource(
        Config::try_from_str(&format!(
            "[subapps.worker]\nconfig_path = {config_path:?}\n"
        ))
        .expect("generated parent config must be valid"),
    );
    match app.try_add_subapp_with_config("worker", |_| {}) {
        Err(error) => error,
        Ok(_) => panic!("broken referenced config must be returned before the builder runs"),
    }
}

fn main() {
    let outcomes = [
        (
            "malformed_inline",
            Config::try_from_str("[worker\nsteps = 1")
                .expect_err("malformed inline TOML must return ConfigError"),
        ),
        (
            "missing_section",
            Config::try_from_str("")
                .expect("empty TOML is valid")
                .try_required_section::<toml::Table>("worker")
                .expect_err("a required section must return ConfigError"),
        ),
        (
            "missing_referenced_file",
            subapp_error("examples/fallible_config_matrix/data/missing.toml"),
        ),
        (
            "malformed_referenced_file",
            subapp_error("examples/fallible_config_matrix/data/malformed.toml"),
        ),
    ];

    let mut passed = 0;
    for (case, error) in outcomes {
        let actual = variant(&error);
        let wanted = expected(case);
        let ok = actual == wanted;
        println!("{case}: expected={wanted} actual={actual} pass={ok}");
        passed += usize::from(ok);
    }
    assert_eq!(passed, CASES.len(), "every error path must remain fallible");
    println!("fallible_config_matrix: passed={passed}/{}", CASES.len());
}
