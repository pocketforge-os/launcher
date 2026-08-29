use pf_catalog::{
    Availability, FavoriteCommitResult, InstalledAppProvider, ManifestErrorKind, ProviderItemResult,
};
use std::{fs, path::Path};
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
    assert!(s.items.iter().any(|i| matches!(
        i.variants[0].availability,
        Availability::NeedsNetwork { .. }
    )));
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
fn unknown_fields_are_typed_invalid_not_drift() {
    let t = tempdir().unwrap();
    let root = t.path().join("apps");
    fs::create_dir(&root).unwrap();
    write(
        &root,
        "drift",
        &manifest(
            "com.example.drift",
            "Drift",
            "pocketforge/a133-powervr",
            "invented=true",
        ),
    );
    let s = provider(&root, &t.path().join("favorites"))
        .snapshot()
        .unwrap();
    assert!(s.items.is_empty());
    assert!(
        matches!(&s.provider_results[0],ProviderItemResult::Invalid{error,..} if error.kind==ManifestErrorKind::Parse)
    );
}
