//! Regression for macro hygiene: consumers need only the documented crates.

use std::fs;
use std::process::Command;

#[test]
fn config_description_needs_no_hidden_dependencies() {
    let derive_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = derive_dir.parent().and_then(|dir| dir.parent()).unwrap();
    let temp = std::env::temp_dir().join(format!(
        "grass-config-description-downstream-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(temp.join("src")).unwrap();
    let path = |relative: &str| {
        root.join(relative)
            .display()
            .to_string()
            .replace('\\', "\\\\")
    };
    fs::write(
        temp.join("Cargo.toml"),
        format!(
            r#"[package]
name = "config-description-downstream"
version = "0.0.0"
edition = "2021"

[dependencies]
grass_io = {{ path = "{}" }}
grass_derive = {{ path = "{}" }}
serde = {{ version = "1", features = ["derive"] }}
"#,
            path("crates/grass_io"),
            path("crates/grass_derive"),
        ),
    )
    .unwrap();
    fs::write(
        temp.join("src/main.rs"),
        r#"use grass_derive::ConfigDescription;
use grass_io::DescribedConfig;
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize, ConfigDescription)]
#[serde(rename_all = "kebab-case")]
enum Mode { #[default] FastMode, AccurateMode }

#[derive(Default, Deserialize, Serialize, ConfigDescription)]
#[serde(rename_all = "kebab-case")]
#[config_description(section = "probe")]
struct ProbeConfig {
    #[serde(default)] mode: Mode,
    #[serde(default)] max_steps: u32,
}

fn main() {
    assert_eq!(ProbeConfig::description().fields[0].choices, ["fast-mode", "accurate-mode"]);
    assert_eq!(ProbeConfig::description().fields[1].name, "max-steps");
}
"#,
    )
    .unwrap();
    let status = Command::new("cargo")
        .args(["check", "--offline"])
        .current_dir(&temp)
        .status()
        .unwrap();
    assert!(status.success(), "minimal documented consumer must compile");

    // A function-valued Serde default can disagree with Default. Metadata must
    // invoke the parser's actual default function rather than guessing.
    fs::write(
        temp.join("src/main.rs"),
        r#"use grass_derive::ConfigDescription;
use serde::{Deserialize, Serialize};

fn parser_default() -> u32 { 7 }

#[derive(Default, Deserialize, Serialize, ConfigDescription)]
#[config_description(section = "probe")]
struct ProbeConfig { #[serde(default = "parser_default")] steps: u32 }

fn main() {
    use grass_io::DescribedConfig;
    assert_eq!(ProbeConfig::description().fields[0].default.as_deref(), Some("7"));
}
"#,
    )
    .unwrap();
    let custom_default = Command::new("cargo")
        .args(["run", "--offline", "--quiet"])
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        custom_default.status.success(),
        "custom Serde default consumer must compile: {}",
        String::from_utf8_lossy(&custom_default.stderr)
    );
    let _ = fs::remove_dir_all(&temp);
}
