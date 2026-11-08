//! W4 acceptance rows (tsp-ziac.4.3.6): the stat-only Ready check over the
//! pf-app-fetch receipt, plus the result.json liveness and reason surfacing.

use fs2::FileExt;
use pf_app_manifest::{fetch_receipt_key, parse_manifest};
use pf_catalog::{Availability, FetchSetupState, InstalledAppProvider};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tempfile::tempdir;

const APP_ID: &str = "com.example.fetch";
const URL: &str = "https://example.com/data.tar.gz";
const URL_CHANGED: &str = "https://example.com/data-v2.tar.gz";
const ARCHIVE_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NETWORK_CLASS: &str =
    "Can't reach the download server. Check the Wi-Fi connection and try again.";
const SETUP_REQUIRED_CLASS: &str = "This app needs to finish setup.";

fn manifest(url: &str, reason: &str) -> String {
    format!(
        r#"schema_version = 3

[app]
id = "{APP_ID}"
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
reason = "{reason}"

[[fetch.files]]
url = "{url}"
sha256 = "{ARCHIVE_SHA}"
size = 3
format = "tar.gz"
dest = "data"
strip_components = 0
"#
    )
}

fn provider(root: &Path, favorites: &Path) -> InstalledAppProvider {
    InstalledAppProvider::new(root, favorites, "pocketforge/a133-powervr", "1")
        .with_platform_version(Some("1".into()))
        .with_supported_capabilities(["input".into()])
}

fn install(root: &Path, text: &str) -> std::path::PathBuf {
    let app = root.join(APP_ID);
    fs::create_dir_all(&app).unwrap();
    fs::write(app.join("app.toml"), text).unwrap();
    app
}

fn shared_key(text: &str) -> String {
    let parsed = parse_manifest(text).unwrap();
    fetch_receipt_key(parsed.fetch.as_ref().unwrap()).unwrap()
}

fn hex_sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn write_payload(app: &Path, files: &[(&str, &[u8])]) -> Value {
    let dest = app.join("data");
    fs::create_dir_all(&dest).unwrap();
    let mut entries = Vec::new();
    let mut total = 0_u64;
    for (path, bytes) in files {
        let target = dest.join(path);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&target, bytes).unwrap();
        total += bytes.len() as u64;
        entries.push(json!({
            "path": path,
            "size": bytes.len(),
            "sha256": hex_sha(bytes),
        }));
    }
    entries.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    json!({
        "dest": "data",
        "archive_sha256": ARCHIVE_SHA,
        "file_count": files.len(),
        "total_bytes": total,
        "manifest": entries,
    })
}

fn write_receipt(app: &Path, key: &str, files: &[Value]) {
    let dir = app.join(".pf-fetch");
    fs::create_dir_all(&dir).unwrap();
    let receipt = json!({
        "schema_version": 1,
        "key": key,
        "files": files,
    });
    fs::write(dir.join("receipt.json"), receipt.to_string()).unwrap();
}

fn write_result(app: &Path, value: &Value) {
    let dir = app.join(".pf-fetch");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("result.json"), value.to_string()).unwrap();
}

fn running_result() -> Value {
    json!({
        "schema_version": 1,
        "run_id": "run-1",
        "app_id": APP_ID,
        "outcome": "running",
        "reason": null,
        "class": null,
        "retryable": null,
        "detail": "",
        "check": "not_run",
        "gets": 0,
    })
}

struct Observed {
    availability: Availability,
    setup: Option<pf_catalog::FetchSetup>,
}

fn observe(root: &Path, state_root: &Path) -> Observed {
    let snapshot = provider(root, &root.join("favorites.json"))
        .with_state_root(state_root)
        .snapshot()
        .unwrap();
    let item = snapshot
        .items
        .iter()
        .find(|item| item.id.ends_with(APP_ID))
        .unwrap();
    let variant = &item.variants[0];
    Observed {
        availability: variant.availability.clone(),
        setup: variant.fetch_setup.clone(),
    }
}

fn needs_setup_reason(availability: &Availability) -> &str {
    match availability {
        Availability::NeedsSetup { reason } => reason,
        other => panic!("expected NeedsSetup, got {other:?}"),
    }
}

#[test]
fn receipt_absent_is_needs_setup() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let app = install(&root, &manifest(URL, "Download"));
    assert!(app.is_dir());
    let seen = observe(&root, &state_root);
    assert_eq!(needs_setup_reason(&seen.availability), "Download");
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::NotStarted);
    assert_eq!(setup.intent.app_id, APP_ID);
    assert_eq!(setup.intent.unit, "pf-app-fetch@com.example.fetch.service");
    assert!(setup.reason.is_none());
}

#[test]
fn valid_receipt_is_ready() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let text = manifest(URL, "Download");
    install(&root, &text);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    write_receipt(&st, &shared_key(&text), &[record]);
    let seen = observe(&root, &state_root);
    assert!(matches!(seen.availability, Availability::Ready));
    assert!(seen.setup.is_none());
}

#[test]
fn stale_key_after_block_change_is_needs_setup() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let before = manifest(URL, "Download");
    let app = install(&root, &before);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    write_receipt(&st, &shared_key(&before), &[record]);
    fs::write(app.join("app.toml"), manifest(URL_CHANGED, "Download")).unwrap();
    let seen = observe(&root, &state_root);
    assert_eq!(needs_setup_reason(&seen.availability), SETUP_REQUIRED_CLASS);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::NotStarted);
    assert_eq!(setup.reason.as_deref(), Some("receipt_stale"));
    assert_eq!(setup.class.as_deref(), Some("setup_required"));
    assert_eq!(setup.message, SETUP_REQUIRED_CLASS);
}

#[test]
fn reason_only_change_stays_ready() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let before = manifest(URL, "Download");
    let app = install(&root, &before);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    write_receipt(&st, &shared_key(&before), &[record]);
    fs::write(app.join("app.toml"), manifest(URL, "Fetching game data")).unwrap();
    let seen = observe(&root, &state_root);
    assert!(
        matches!(seen.availability, Availability::Ready),
        "{:?}",
        seen.availability
    );
}

#[test]
fn missing_dest_is_needs_setup() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let text = manifest(URL, "Download");
    install(&root, &text);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    write_receipt(&st, &shared_key(&text), &[record]);
    fs::remove_dir_all(st.join("data")).unwrap();
    let seen = observe(&root, &state_root);
    assert_eq!(needs_setup_reason(&seen.availability), SETUP_REQUIRED_CLASS);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.reason.as_deref(), Some("state_invalid"));
    assert_eq!(setup.state, FetchSetupState::NotStarted);
}

#[test]
fn file_count_mismatch_is_needs_setup() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let text = manifest(URL, "Download");
    install(&root, &text);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    write_receipt(&st, &shared_key(&text), &[record]);
    fs::write(st.join("data/extra.txt"), b"extra").unwrap();
    let seen = observe(&root, &state_root);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.reason.as_deref(), Some("state_invalid"));
    assert!(!matches!(seen.availability, Availability::Ready));
}

#[test]
fn bytes_mismatch_is_needs_setup() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let text = manifest(URL, "Download");
    install(&root, &text);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    write_receipt(&st, &shared_key(&text), &[record]);
    fs::write(st.join("data/one.txt"), b"abcd").unwrap();
    let seen = observe(&root, &state_root);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.reason.as_deref(), Some("state_invalid"));
    assert!(!matches!(seen.availability, Availability::Ready));
}

#[test]
fn failed_result_maps_reason_to_class_text() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    install(&root, &manifest(URL, "Download"));
    write_result(
        &st,
        &json!({
            "schema_version": 1,
            "run_id": "run-7",
            "app_id": APP_ID,
            "outcome": "failed",
            "reason": "offline",
            "class": "network",
            "retryable": true,
            "detail": "connect refused",
            "check": "download",
            "gets": 1,
        }),
    );
    let seen = observe(&root, &state_root);
    assert_eq!(needs_setup_reason(&seen.availability), NETWORK_CLASS);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::Failed);
    assert_eq!(setup.reason.as_deref(), Some("offline"));
    assert_eq!(setup.class.as_deref(), Some("network"));
    assert_eq!(setup.message, NETWORK_CLASS);
    assert!(setup.retry_offered);
    assert_eq!(setup.run_id.as_deref(), Some("run-7"));
}

#[test]
fn running_with_lock_held_is_setting_up() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    install(&root, &manifest(URL, "Download"));
    write_result(&st, &running_result());
    let lock_path = st.join(".pf-fetch/lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .unwrap();
    lock.lock_exclusive().unwrap();
    let seen = observe(&root, &state_root);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::SettingUp);
    assert!(!setup.retry_offered);
    assert_eq!(setup.message, "Setting up…");
    drop(lock);
}

#[test]
fn running_with_lock_free_is_interrupted_with_retry() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    install(&root, &manifest(URL, "Download"));
    write_result(&st, &running_result());
    fs::create_dir_all(st.join(".pf-fetch")).unwrap();
    fs::write(st.join(".pf-fetch/lock"), b"").unwrap();
    let seen = observe(&root, &state_root);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::Interrupted);
    assert!(setup.retry_offered);
    assert_eq!(setup.message, "Setup was interrupted.");
    assert!(!matches!(seen.availability, Availability::Ready));
}

#[test]
fn state_under_descriptor_dir_does_not_count() {
    // The producer keeps state under the state root; a payload and receipt
    // placed next to app.toml must never make the app Ready.
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let text = manifest(URL, "Download");
    let app = install(&root, &text);
    let record = write_payload(&app, &[("one.txt", b"abc")]);
    write_receipt(&app, &shared_key(&text), &[record]);
    let seen = observe(&root, &state_root);
    assert_eq!(needs_setup_reason(&seen.availability), "Download");
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::NotStarted);
}

#[test]
fn default_state_root_is_var_lib_pocketforge_apps() {
    // Without with_state_root the provider must not look next to app.toml.
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let text = manifest(URL, "Download");
    let app = install(&root, &text);
    let record = write_payload(&app, &[("one.txt", b"abc")]);
    write_receipt(&app, &shared_key(&text), &[record]);
    let snapshot = provider(&root, &root.join("favorites.json"))
        .snapshot()
        .unwrap();
    let variant = &snapshot.items[0].variants[0];
    assert!(matches!(
        variant.availability,
        Availability::NeedsSetup { .. }
    ));
}

#[test]
fn running_with_unopenable_lock_is_interrupted_with_retry() {
    // A lock that cannot be opened (here a symlink loop, ELOOP, which also fails
    // as root) is not proof of a live run: never "setting up forever".
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    install(&root, &manifest(URL, "Download"));
    write_result(&st, &running_result());
    std::os::unix::fs::symlink("lock", st.join(".pf-fetch/lock")).unwrap();
    let seen = observe(&root, &state_root);
    let setup = seen.setup.expect("fetch setup intent");
    assert_eq!(setup.state, FetchSetupState::Interrupted);
    assert!(setup.retry_offered);
}

#[test]
fn receipt_missing_a_declared_dest_is_needs_setup() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    let state_root = t.path().join("state");
    let st = state_root.join(APP_ID);
    let text = manifest(URL, "Download");
    install(&root, &text);
    let record = write_payload(&st, &[("one.txt", b"abc")]);
    let mut other = record.clone();
    other["dest"] = json!("other");
    // Receipt carries an extra entry whose payload is valid but undeclared.
    fs::create_dir_all(st.join("other")).unwrap();
    fs::write(st.join("other/one.txt"), b"abc").unwrap();
    write_receipt(&st, &shared_key(&text), &[record.clone(), other]);
    let seen = observe(&root, &state_root);
    assert_eq!(
        seen.setup.expect("fetch setup intent").reason.as_deref(),
        Some("state_invalid")
    );
    // And the opposite: a receipt with no entry for the declared dest.
    write_receipt(&st, &shared_key(&text), &[]);
    let seen = observe(&root, &state_root);
    assert!(!matches!(seen.availability, Availability::Ready));
    // A receipt entry under a different dest than declared.
    let mut renamed = record;
    renamed["dest"] = json!("other");
    write_receipt(&st, &shared_key(&text), &[renamed]);
    let seen = observe(&root, &state_root);
    assert_eq!(
        seen.setup.expect("fetch setup intent").reason.as_deref(),
        Some("state_invalid")
    );
}
