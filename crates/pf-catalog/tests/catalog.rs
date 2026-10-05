use pf_catalog::{
    Availability, CatalogSnapshot, FavoriteCommitResult, InstalledAppProvider, ManifestErrorKind,
    ProviderItemResult,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Barrier},
    thread,
};
use tempfile::tempdir;

fn manifest(id: &str, title: &str, family: &str, extra: &str) -> String {
    format!(
        r#"[app]
id="{id}"
name="{title}"
category="game"
version="1.0.0"
use=["input"]
[runtime]
family="{family}"
abi="1"
platform-version="1"
[launch]
exec="./launch"
{extra}
"#
    )
}
fn write(root: &Path, dir: &str, value: &str) {
    let p = root.join(dir);
    fs::create_dir_all(&p).unwrap();
    fs::write(p.join("app.toml"), value).unwrap();
}
fn provider(root: &Path, state: &Path) -> InstalledAppProvider {
    InstalledAppProvider::new(root, state, "pocketforge/a133-powervr", "1")
        .with_platform_version(Some("1".into()))
        .with_supported_capabilities(["input".into()])
}

#[test]
fn all_typed_states_and_duplicate_titles_are_preserved() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    write(
        &root,
        "ready",
        &manifest("com.example.ready", "Same", "pocketforge/a133-powervr", ""),
    );
    write(
        &root,
        "network",
        &manifest(
            "com.example.network",
            "Same",
            "pocketforge/a133-powervr",
            "needs_network=true",
        ),
    );
    write(
        &root,
        "setup",
        &format!(
            "{}\n[fetch]\nenabled=true\nreason=\"Download\"",
            manifest("com.example.setup", "Setup", "pocketforge/a133-powervr", "")
        ),
    );
    write(
        &root,
        "other",
        &manifest("com.example.other", "Other", "pocketforge/a523-mali", ""),
    );
    write(&root, "corrupt", "bad=[");
    fs::create_dir(root.join("missing")).unwrap();
    let s = provider(&root, &t.path().join("favorites"))
        .snapshot()
        .unwrap();
    assert_eq!(s.items.len(), 4);
    assert_eq!(s.items.iter().filter(|i| i.title == "Same").count(), 2);
    assert!(
        s.items
            .iter()
            .filter(|i| i.title == "Same")
            .all(|i| i.variants[0].provenance.provider_id == "installed-applications")
    );
    let network = s
        .items
        .iter()
        .find(|item| item.id.ends_with("com.example.network"))
        .unwrap();
    assert!(matches!(
        network.variants[0].availability,
        Availability::Ready
    ));
    assert!(network.variants[0].needs_network);
    assert!(
        s.provider_results
            .iter()
            .any(|r| matches!(r, ProviderItemResult::SetupRequired { .. }))
    );
    assert!(
        s.provider_results
            .iter()
            .any(|r| matches!(r, ProviderItemResult::Incompatible { .. }))
    );
    assert_eq!(
        s.provider_results
            .iter()
            .filter(|r| matches!(r, ProviderItemResult::Invalid { .. }))
            .count(),
        2
    );
    assert!(s.provider_results.iter().any(|r|matches!(r,ProviderItemResult::Invalid{error,..} if error.kind==ManifestErrorKind::Missing)));
}

#[test]
fn generated_five_hundred_item_fixture_is_deterministic() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    for n in (0..500).rev() {
        write(
            &root,
            &format!("app-{n:03}"),
            &manifest(
                &format!("com.example.app{n:03}"),
                &format!("App {n:03}"),
                "pocketforge/a133-powervr",
                "",
            ),
        );
    }
    let p = provider(&root, &t.path().join("favorites"));
    let a = p.snapshot().unwrap();
    let b = p.snapshot().unwrap();
    assert_eq!(a, b);
    assert_eq!(a.items.len(), 500);
    assert!(a.items.windows(2).all(|w| w[0].id < w[1].id));
}

#[test]
fn favorites_are_atomic_revisioned_and_persistent() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    let state = t.path().join("state/favorites.json");
    write(
        &root,
        "one",
        &manifest("com.example.one", "One", "pocketforge/a133-powervr", ""),
    );
    let p = provider(&root, &state);
    let first = p.snapshot().unwrap();
    let id = first.items[0].id.clone();
    assert!(matches!(
        p.set_favorite(&id, true, first.revision).unwrap(),
        FavoriteCommitResult::Committed(_)
    ));
    let favored = p.snapshot().unwrap();
    assert_ne!(first.revision, favored.revision);
    assert_eq!(
        favored.user_projection.favorite_item_ids.as_slice(),
        std::slice::from_ref(&id)
    );
    assert!(
        matches!(p.set_favorite(&id,false,first.revision).unwrap(),FavoriteCommitResult::RevisionConflict{current} if current==favored.revision)
    );
    write(
        &root,
        "two",
        &manifest("com.example.two", "Two", "pocketforge/a133-powervr", ""),
    );
    let refreshed = p.snapshot().unwrap();
    assert_ne!(refreshed.revision, favored.revision);
    assert_eq!(refreshed.user_projection.favorite_item_ids, [id]);
    assert!(
        !fs::read_to_string(root.join("one/app.toml"))
            .unwrap()
            .contains("favorite")
    );
}

#[test]
fn variant_pins_are_cas_backed_and_persist_in_the_catalog_projection() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    let state = t.path().join("state/catalog-overlay.json");
    write(
        &root,
        "one",
        &manifest("com.example.one", "One", "pocketforge/a133-powervr", ""),
    );
    let p = provider(&root, &state);
    let first = p.snapshot().unwrap();
    let item_id = first.items[0].id.clone();
    let variant_id = first.items[0].variants[0].id.clone();
    assert!(matches!(
        p.set_pinned_variant(&item_id, Some(&variant_id), first.revision)
            .unwrap(),
        pf_catalog::VariantPinCommitResult::Committed(_)
    ));
    let pinned = provider(&root, &state).snapshot().unwrap();
    assert_eq!(
        pinned.user_projection.pinned_variant_ids.get(&item_id),
        Some(&variant_id)
    );
    assert!(matches!(
        p.set_pinned_variant(&item_id, None, first.revision)
            .unwrap(),
        pf_catalog::VariantPinCommitResult::RevisionConflict { .. }
    ));
    assert!(matches!(
        p.set_pinned_variant(&item_id, None, pinned.revision)
            .unwrap(),
        pf_catalog::VariantPinCommitResult::Committed(_)
    ));
    assert!(
        provider(&root, &state)
            .snapshot()
            .unwrap()
            .user_projection
            .pinned_variant_ids
            .is_empty()
    );
}

#[test]
fn concurrent_favorite_commits_compare_and_swap() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    let state = t.path().join("state/favorites.json");
    for id in ["one", "two"] {
        write(
            &root,
            id,
            &manifest(
                &format!("com.example.{id}"),
                id,
                "pocketforge/a133-powervr",
                "",
            ),
        );
    }

    let provider = Arc::new(provider(&root, &state));
    let initial = provider.snapshot().unwrap();
    let ids: Vec<_> = initial.items.iter().map(|item| item.id.clone()).collect();
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = ids
        .iter()
        .cloned()
        .map(|id| {
            let provider = Arc::clone(&provider);
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                (
                    id.clone(),
                    provider.set_favorite(&id, true, initial.revision).unwrap(),
                )
            })
        })
        .collect();

    barrier.wait();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    assert_eq!(
        results
            .iter()
            .filter(|(_, result)| matches!(result, FavoriteCommitResult::Committed(_)))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|(_, result)| matches!(result, FavoriteCommitResult::RevisionConflict { .. }))
            .count(),
        1
    );
    let winner = results
        .iter()
        .find_map(|(id, result)| matches!(result, FavoriteCommitResult::Committed(_)).then_some(id))
        .unwrap();
    let final_snapshot = provider.snapshot().unwrap();
    assert_eq!(
        final_snapshot.user_projection.favorite_item_ids.as_slice(),
        std::slice::from_ref(winner)
    );
    assert!(
        fs::read_dir(state.parent().unwrap())
            .unwrap()
            .flatten()
            .all(|entry| !entry.file_name().to_string_lossy().contains(".tmp."))
    );
}

#[test]
fn unknown_app_field_is_refused_by_shared_manifest_parser() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    write(
        &root,
        "drift",
        &manifest("com.example.drift", "Drift", "pocketforge/a133-powervr", "")
            .replace("category=\"game\"", "category=\"game\"\ntheme=\"dark\""),
    );
    let s = provider(&root, &t.path().join("favorites"))
        .snapshot()
        .unwrap();
    assert_eq!(s.items, []);
    assert!(
        matches!(&s.provider_results[0],ProviderItemResult::Invalid{error,..} if error.kind==ManifestErrorKind::Validation)
    );
}

fn poolsuite_manifest() -> String {
    r#"[app]
id="org.pocketforge.poolsuite"
name="Poolsuite"
category="media"
version="1.0.0"
use=["input", "audio"]
[runtime]
family="pocketforge/a133-powervr"
abi="1"
platform-version="20"
[launch]
exec="bin/poolsuite"
needs_network=true
audio=true
"#
    .into()
}

fn scan_a133(source: &str) -> CatalogSnapshot {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    write(&root, "poolsuite", source);
    InstalledAppProvider::new(
        &root,
        t.path().join("favorites"),
        "pocketforge/a133-powervr",
        "1",
    )
    .with_platform_version(Some("20".into()))
    .with_supported_capabilities(["audio".into(), "input".into()])
    .snapshot()
    .unwrap()
}

fn compatibility_reason(snapshot: &CatalogSnapshot) -> &str {
    match &snapshot.provider_results[0] {
        ProviderItemResult::Incompatible { reason, .. } => reason,
        result => panic!("expected incompatible result, got {result:?}"),
    }
}

#[test]
fn a133_open_poolsuite_descriptor_is_ready_with_network_cue_metadata() {
    let snapshot = scan_a133(&poolsuite_manifest());
    let variant = &snapshot.items[0].variants[0];

    assert!(matches!(variant.availability, Availability::Ready));
    assert!(variant.needs_network);
    assert!(matches!(
        snapshot.provider_results[0],
        ProviderItemResult::Valid { .. }
    ));
}

#[test]
fn runtime_family_mismatch_reports_stable_reason_code() {
    let snapshot = scan_a133(
        &poolsuite_manifest().replace("pocketforge/a133-powervr", "pocketforge/a523-mali"),
    );

    assert_eq!(compatibility_reason(&snapshot), "runtime_family_mismatch");
}

#[test]
fn runtime_abi_mismatch_reports_stable_reason_code() {
    let snapshot = scan_a133(&poolsuite_manifest().replace("abi=\"1\"", "abi=\"2\""));

    assert_eq!(compatibility_reason(&snapshot), "runtime_abi_mismatch");
}

#[test]
fn platform_version_mismatch_reports_stable_reason_code() {
    let snapshot = scan_a133(
        &poolsuite_manifest().replace("platform-version=\"20\"", "platform-version=\"21\""),
    );

    assert_eq!(compatibility_reason(&snapshot), "platform_version_mismatch");
}

#[test]
fn unsupported_required_capability_reports_stable_reason_code() {
    let snapshot = scan_a133(&poolsuite_manifest().replace(
        "use=[\"input\", \"audio\"]",
        "use=[\"input\", \"audio\", \"settings\"]",
    ));

    assert_eq!(compatibility_reason(&snapshot), "unsupported_capability");
    assert!(matches!(
        snapshot.items[0].variants[0].availability,
        Availability::UnsupportedCapability { .. }
    ));
}

#[test]
fn unsupported_optional_capability_does_not_block() {
    let snapshot = scan_a133(&poolsuite_manifest().replace(
        "use=[\"input\", \"audio\"]",
        "use=[\"input\", \"audio\", \"settings?\"]",
    ));

    assert!(matches!(
        snapshot.items[0].variants[0].availability,
        Availability::Ready
    ));
}

#[test]
fn invalid_app_id_is_refused_by_shared_manifest_parser() {
    let snapshot =
        scan_a133(&poolsuite_manifest().replace("org.pocketforge.poolsuite", "org..poolsuite"));

    assert_eq!(snapshot.items, []);
    assert!(matches!(
        &snapshot.provider_results[0],
        ProviderItemResult::Invalid { error, .. }
            if error.kind == ManifestErrorKind::Validation
                && error.message.contains("invalid app.id")
    ));
}

#[test]
fn snapshot_without_needs_network_deserializes_as_false() {
    let snapshot = scan_a133(&poolsuite_manifest());
    let mut value = serde_json::to_value(snapshot).unwrap();
    value["items"][0]["variants"][0]
        .as_object_mut()
        .unwrap()
        .remove("needs_network");

    let restored: CatalogSnapshot = serde_json::from_value(value).unwrap();

    assert!(!restored.items[0].variants[0].needs_network);
}
