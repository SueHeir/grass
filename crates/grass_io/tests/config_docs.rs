//! `App::config_reference` renders every plugin's sections as Markdown.

use grass_app::prelude::*;
use grass_io::{CheckpointPlugin, Config, RunPlugin, TermOutPlugin};

#[test]
fn reference_lists_every_section_with_its_fields() {
    let mut app = App::new();
    app.add_resource(Config::from_str(""));
    app.add_plugins(CheckpointPlugin);
    app.add_plugins(TermOutPlugin);
    app.add_plugins(RunPlugin);
    let md = app.config_reference();
    if std::env::var("SHOW_MD").is_ok() {
        println!("{md}");
    }

    for section in ["### `[clock]`", "### `[checkpoint]`", "### `[term_out]`"] {
        assert!(md.contains(section), "missing {section}:\n{md}");
    }
    // A field row: key, type, default, description.
    let row = md
        .lines()
        .find(|l| l.starts_with("| `every` |") && l.contains("checkpoint"))
        .unwrap_or_else(|| panic!("no checkpoint `every` row:\n{md}"));
    assert!(row.contains("`0`"), "{row}");
    assert!(md.contains("| `keep` |"));
    assert!(md.contains("_Read by `"));
    // Sections appear in registration order: clock (pulled in by the
    // checkpoint plugin) is registered before checkpoint.
    let clock = md.find("### `[clock]`").unwrap();
    let ckpt = md.find("### `[checkpoint]`").unwrap();
    assert!(clock < ckpt);
}

#[test]
fn app_without_described_plugins_says_so() {
    let app = App::new();
    assert!(app
        .config_reference()
        .contains("No plugin in this app reads"));
}
