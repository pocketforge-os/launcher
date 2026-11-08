//! Reason-code surface: every W0 code maps to its TOML class and every class
//! text is the TOML text verbatim. The fixture is a vendored copy of
//! mission-control `.planning/infra/infra-107.4-fetch-reason-codes.toml`
//! @ 43c34a0b (see the comment at its top).

use pf_catalog::{Availability, FetchSetupState, InstalledAppProvider};
use serde_json::json;
use std::fs;
use tempfile::tempdir;

const TOML_TEXT: &str = include_str!("fixtures/infra-107.4-fetch-reason-codes.toml");
const APP_ID: &str = "com.example.fetch";

const MANIFEST: &str = r#"schema_version = 3

[app]
id = "com.example.fetch"
name = "Fetched"
category = "game"
version = "1.0.0"
use = ["input"]

[runtime]
family = "pocketforge/a133-powervr"
abi = "1"
platform-version = "1"

[launch]
exec = "./launch"

[fetch]
enabled = true
reason = "Download"

[[fetch.files]]
url = "https://example.com/data.tar.gz"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
size = 3
format = "tar.gz"
dest = "data"
strip_components = 0
"#;

/// Runs one failed `result.json` through the real snapshot pipeline.
fn failed(reason: &str, result_class: Option<&str>) -> (Availability, pf_catalog::FetchSetup) {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    fs::create_dir_all(root.join(APP_ID)).unwrap();
    fs::write(root.join(APP_ID).join("app.toml"), MANIFEST).unwrap();
    let state = state_root.join(APP_ID).join(".pf-fetch");
    fs::create_dir_all(&state).unwrap();
    fs::write(
        state.join("result.json"),
        json!({
            "schema_version": 1, "run_id": "r1", "app_id": APP_ID,
            "outcome": "failed", "reason": reason, "class": result_class,
            "retryable": false, "detail": "", "check": "download", "gets": 1,
        })
        .to_string(),
    )
    .unwrap();
    let snapshot = InstalledAppProvider::new(
        &root,
        root.join("favorites.json"),
        "pocketforge/a133-powervr",
        "1",
    )
    .with_platform_version(Some("1".into()))
    .with_supported_capabilities(["input".into()])
    .with_state_root(&state_root)
    .snapshot()
    .unwrap();
    let variant = &snapshot.items[0].variants[0];
    (
        variant.availability.clone(),
        variant.fetch_setup.clone().expect("fetch setup"),
    )
}

#[test]
fn every_toml_code_maps_to_its_class_and_class_text() {
    let doc: toml::Table = TOML_TEXT.parse().unwrap();
    let classes = doc["classes"].as_table().unwrap();
    let codes = doc["codes"].as_table().unwrap();
    assert!(codes.len() >= 21, "fixture lost codes: {}", codes.len());
    for (code, entry) in codes {
        let class = entry["class"].as_str().unwrap();
        let text = classes[class].as_str().unwrap();
        // result.json carries no class, so the mapping must come from the code.
        let (availability, setup) = failed(code, None);
        assert_eq!(setup.state, FetchSetupState::Failed, "{code}");
        assert_eq!(setup.class.as_deref(), Some(class), "{code}");
        assert_eq!(setup.message, text, "{code}");
        assert!(setup.retry_offered, "{code}: a retry is always offered");
        assert_eq!(
            availability,
            Availability::NeedsSetup {
                reason: text.into()
            },
            "{code}"
        );
    }
}

#[test]
fn every_toml_class_text_is_surfaced_verbatim() {
    let doc: toml::Table = TOML_TEXT.parse().unwrap();
    let classes = doc["classes"].as_table().unwrap();
    assert_eq!(classes.len(), 10);
    for (class, text) in classes {
        // An unknown code that carries a known class uses that class's text.
        let (_, setup) = failed("future_code", Some(class));
        assert_eq!(setup.message, text.as_str().unwrap(), "{class}");
        assert_eq!(setup.class.as_deref(), Some(class.as_str()));
    }
}

#[test]
fn unknown_code_falls_back_with_retry() {
    let (availability, setup) = failed("future_code", None);
    assert_eq!(setup.state, FetchSetupState::Failed);
    assert_eq!(setup.reason.as_deref(), Some("future_code"));
    assert_eq!(setup.class, None);
    assert_eq!(setup.message, "Setup failed. Try again.");
    assert!(setup.retry_offered);
    assert!(matches!(availability, Availability::NeedsSetup { .. }));
    let (_, setup) = failed("future_code", Some("future_class"));
    assert_eq!(setup.message, "Setup failed. Try again.");
    assert!(setup.retry_offered);
}
