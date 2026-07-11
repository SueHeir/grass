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
        r##"use grass_derive::ConfigDescription;
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
"##,
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

    // `Default` alone does not make a field optional to Serde. The generated
    // sample must keep it as a placeholder and preserve Serde's error when it
    // is omitted.
    fs::write(
        temp.join("src/main.rs"),
        r##"use grass_derive::ConfigDescription;
use grass_io::{Config, DescribedConfig};
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize, ConfigDescription)]
#[config_description(section = "probe")]
struct ProbeConfig { required_name: String }

fn main() {
    let description = ProbeConfig::description();
    let field = &description.fields[0];
    assert!(field.required);
    assert_eq!(field.default, None);
    let sample = description.render_toml();
    assert!(sample.contains("# required_name = <required string>"));
    assert!(!sample.contains("\nrequired_name ="));
    assert!(Config::from_str(&sample).try_section::<ProbeConfig>("probe").is_err());
}
"##,
    )
    .unwrap();
    let required_field = Command::new("cargo")
        .args(["run", "--offline", "--quiet"])
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        required_field.status.success(),
        "required-field consumer must preserve Serde rejection: {}",
        String::from_utf8_lossy(&required_field.stderr)
    );

    // Members Serde deliberately does not deserialize are implementation
    // state, not TOML fields.  Showing one in the reference would invite a
    // key that either errors under deny_unknown_fields or is ignored.
    fs::write(
        temp.join("src/main.rs"),
        r##"use grass_derive::ConfigDescription;
use grass_io::DescribedConfig;
use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize, ConfigDescription)]
#[serde(deny_unknown_fields)]
#[config_description(section = "probe")]
struct ProbeConfig {
    #[serde(default)] visible: u32,
    #[serde(skip)] cached_value: String,
    #[serde(skip_deserializing)] runtime_note: String,
}

fn main() {
    let description = ProbeConfig::description();
    assert_eq!(description.fields.len(), 1);
    assert_eq!(description.fields[0].name, "visible");
    let rendered = description.render_toml();
    assert!(!rendered.contains("cached_value"));
    assert!(!rendered.contains("runtime_note"));
}
"##,
    )
    .unwrap();
    let skipped_fields = Command::new("cargo")
        .args(["run", "--offline", "--quiet"])
        .current_dir(&temp)
        .output()
        .unwrap();
    assert!(
        skipped_fields.status.success(),
        "non-deserialized fields must not be advertised: {}",
        String::from_utf8_lossy(&skipped_fields.stderr)
    );
    let _ = fs::remove_dir_all(&temp);
}
