//! Installed-application catalog with immutable snapshots and a separate favorites overlay.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use thiserror::Error;

static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub type CatalogRevision = u64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogSnapshot {
    pub revision: CatalogRevision,
    pub observed_at_unix_seconds: u64,
    pub provider_results: Vec<ProviderItemResult>,
    pub items: Vec<CatalogItem>,
    pub user_projection: UserProjection,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CatalogItem {
    pub id: String,
    pub title: String,
    pub kind: AppKind,
    pub presentation: Presentation,
    pub tags: Vec<String>,
    pub variants: Vec<Variant>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Presentation {
    pub icon_reference: Option<String>,
    #[serde(default)]
    pub icon_decodable: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppKind {
    Media,
    Stream,
    Game,
    System,
    Settings,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Variant {
    pub id: String,
    pub provider_id: String,
    pub availability: Availability,
    pub requirements: Vec<Requirement>,
    pub provenance: Provenance,
    pub launch_target: AppManifestRef,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AppManifestRef {
    pub app_id: String,
    pub descriptor_path: PathBuf,
    pub observed_digest: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub provider_id: String,
    pub app_version: Option<String>,
    pub upstream_version: Option<String>,
    pub runtime_family: String,
    pub runtime_abi: String,
    pub platform_version: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Requirement {
    pub capability: String,
    pub optional: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Availability {
    Ready,
    NeedsNetwork { reason: String },
    NeedsSetup { reason: String },
    UnsupportedCapability { capability: String },
    IncompatibleRuntime { required: String, available: String },
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ProviderItemResult {
    Valid {
        item_id: String,
    },
    Invalid {
        descriptor_path: PathBuf,
        error: ManifestError,
    },
    Incompatible {
        item_id: String,
        required: String,
    },
    NetworkRequired {
        item_id: String,
    },
    SetupRequired {
        item_id: String,
    },
}
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UserProjection {
    pub favorite_item_ids: Vec<String>,
    #[serde(default)]
    pub pinned_variant_ids: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FavoriteCommitResult {
    Committed(CatalogRevision),
    RevisionConflict { current: CatalogRevision },
    ItemNotFound,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VariantPinCommitResult {
    Committed(CatalogRevision),
    RevisionConflict { current: CatalogRevision },
    ItemNotFound,
    VariantNotFound,
}
#[derive(Clone, Debug, Eq, Error, PartialEq, Serialize, Deserialize)]
#[error("{kind:?}: {message}")]
pub struct ManifestError {
    pub kind: ManifestErrorKind,
    pub message: String,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestErrorKind {
    Missing,
    Parse,
    Validation,
}
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("projection: {0}")]
    Projection(#[from] serde_json::Error),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    app: App,
    runtime: Runtime,
    launch: Option<Launch>,
    health: Option<Health>,
    fetch: Option<Fetch>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct App {
    id: String,
    name: Option<String>,
    category: Option<AppKind>,
    order: Option<i64>,
    icon: Option<String>,
    version: Option<String>,
    upstream_version: Option<String>,
    #[serde(default, rename = "use")]
    capabilities: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Runtime {
    family: String,
    abi: String,
    #[serde(rename = "platform-version")]
    platform_version: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Launch {
    exec: String,
    #[serde(default)]
    needs_network: bool,
    #[serde(default)]
    takes_display: bool,
    #[serde(default)]
    audio: bool,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Health {
    preflight: Option<String>,
    timeout_sec: Option<u64>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Fetch {
    enabled: bool,
    destination: Option<String>,
    reason: Option<String>,
    files: Option<Vec<FetchFile>>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FetchFile {
    url: String,
    sha256: String,
    size: Option<u64>,
    format: Option<String>,
    dest: Option<String>,
    strip_components: Option<u64>,
    executable: Option<std::collections::BTreeMap<String, String>>,
}

pub struct InstalledAppProvider {
    root: PathBuf,
    favorites: PathBuf,
    runtime_family: String,
    runtime_abi: String,
    capabilities: BTreeSet<String>,
    observed_at: u64,
}
impl InstalledAppProvider {
    #[must_use]
    pub fn new(
        root: impl Into<PathBuf>,
        favorites: impl Into<PathBuf>,
        family: impl Into<String>,
        abi: impl Into<String>,
    ) -> Self {
        Self {
            root: root.into(),
            favorites: favorites.into(),
            runtime_family: family.into(),
            runtime_abi: abi.into(),
            capabilities: BTreeSet::new(),
            observed_at: 0,
        }
    }
    #[must_use]
    pub fn with_supported_capabilities(mut self, values: impl IntoIterator<Item = String>) -> Self {
        self.capabilities = values.into_iter().collect();
        self
    }
    #[must_use]
    pub fn with_observed_at(mut self, value: u64) -> Self {
        self.observed_at = value;
        self
    }
    /// Returns a complete immutable view.
    ///
    /// # Errors
    /// Returns an error when the app root or favorites projection cannot be read.
    pub fn snapshot(&self) -> Result<CatalogSnapshot, ProviderError> {
        self.scan(self.load_projection()?)
    }
    /// Atomically changes the PocketForge-owned projection using optimistic concurrency.
    ///
    /// # Errors
    /// Returns an error when the catalog cannot be scanned or the projection committed.
    pub fn set_favorite(
        &self,
        id: &str,
        value: bool,
        expected: CatalogRevision,
    ) -> Result<FavoriteCommitResult, ProviderError> {
        let parent = self.favorites.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.favorites.with_extension("lock"))?;
        lock.lock_exclusive()?;

        let current = self.snapshot()?;
        if current.revision != expected {
            return Ok(FavoriteCommitResult::RevisionConflict {
                current: current.revision,
            });
        }
        if !current.items.iter().any(|i| i.id == id) {
            return Ok(FavoriteCommitResult::ItemNotFound);
        }
        let mut p = current.user_projection;
        match p.favorite_item_ids.binary_search_by(|x| x.as_str().cmp(id)) {
            Ok(i) if !value => {
                p.favorite_item_ids.remove(i);
            }
            Err(i) if value => p.favorite_item_ids.insert(i, id.into()),
            _ => {}
        }
        self.store_projection(&p)?;
        Ok(FavoriteCommitResult::Committed(self.scan(p)?.revision))
    }
    /// Atomically pins (or clears) a title's default variant using the catalog projection CAS.
    ///
    /// # Errors
    /// Returns an error when the catalog cannot be scanned or the projection committed.
    pub fn set_pinned_variant(
        &self,
        item_id: &str,
        variant_id: Option<&str>,
        expected: CatalogRevision,
    ) -> Result<VariantPinCommitResult, ProviderError> {
        let parent = self.favorites.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.favorites.with_extension("lock"))?;
        lock.lock_exclusive()?;
        let current = self.snapshot()?;
        if current.revision != expected {
            return Ok(VariantPinCommitResult::RevisionConflict {
                current: current.revision,
            });
        }
        let Some(item) = current.items.iter().find(|item| item.id == item_id) else {
            return Ok(VariantPinCommitResult::ItemNotFound);
        };
        if variant_id.is_some_and(|id| !item.variants.iter().any(|variant| variant.id == id)) {
            return Ok(VariantPinCommitResult::VariantNotFound);
        }
        let mut projection = current.user_projection;
        match variant_id {
            Some(id) => {
                projection
                    .pinned_variant_ids
                    .insert(item_id.into(), id.into());
            }
            None => {
                projection.pinned_variant_ids.remove(item_id);
            }
        }
        self.store_projection(&projection)?;
        Ok(VariantPinCommitResult::Committed(
            self.scan(projection)?.revision,
        ))
    }
    fn load_projection(&self) -> Result<UserProjection, ProviderError> {
        match fs::read(&self.favorites) {
            Ok(b) => Ok(serde_json::from_slice(&b)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserProjection::default()),
            Err(e) => Err(e.into()),
        }
    }
    fn store_projection(&self, p: &UserProjection) -> Result<(), ProviderError> {
        let parent = self.favorites.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .favorites
            .with_extension(format!("tmp.{}.{sequence}", std::process::id()));
        let result = (|| {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&tmp)?;
            f.write_all(&serde_json::to_vec(p)?)?;
            f.sync_all()?;
            fs::rename(&tmp, &self.favorites)?;
            Ok::<_, ProviderError>(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    }
    fn scan(&self, mut projection: UserProjection) -> Result<CatalogSnapshot, ProviderError> {
        projection.favorite_item_ids.sort();
        projection.favorite_item_ids.dedup();
        let mut dirs: Vec<_> = fs::read_dir(&self.root)?
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        let (mut items, mut results) = (vec![], vec![]);
        for d in dirs {
            self.scan_one(&d, &mut items, &mut results);
        }
        items.sort_by(|a, b| a.id.cmp(&b.id));
        results.sort_by_key(result_key);
        let bytes = serde_json::to_vec(&(&items, &results, &projection))?;
        let hash = Sha256::digest(bytes);
        Ok(CatalogSnapshot {
            revision: u64::from_be_bytes(hash[..8].try_into().expect("hash prefix")),
            observed_at_unix_seconds: self.observed_at,
            provider_results: results,
            items,
            user_projection: projection,
        })
    }
    #[allow(clippy::too_many_lines)]
    fn scan_one(
        &self,
        dir: &Path,
        items: &mut Vec<CatalogItem>,
        results: &mut Vec<ProviderItemResult>,
    ) {
        let path = dir.join("app.toml");
        let bytes = match fs::read(&path) {
            Ok(v) => v,
            Err(e) => {
                results.push(ProviderItemResult::Invalid {
                    descriptor_path: path,
                    error: ManifestError {
                        kind: if e.kind() == std::io::ErrorKind::NotFound {
                            ManifestErrorKind::Missing
                        } else {
                            ManifestErrorKind::Parse
                        },
                        message: e.to_string(),
                    },
                });
                return;
            }
        };
        let text = match std::str::from_utf8(&bytes) {
            Ok(v) => v,
            Err(e) => {
                results.push(invalid(path, e.to_string(), ManifestErrorKind::Parse));
                return;
            }
        };
        let m: Manifest = match toml::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                results.push(invalid(path, e.to_string(), ManifestErrorKind::Parse));
                return;
            }
        };
        if let Err(e) = validate(&m) {
            results.push(ProviderItemResult::Invalid {
                descriptor_path: path,
                error: e,
            });
            return;
        }
        let id = format!("installed-applications:{}", m.app.id);
        let incompatible =
            m.runtime.family != self.runtime_family || m.runtime.abi != self.runtime_abi;
        let unsupported = m.app.capabilities.iter().find_map(|c| {
            let optional = c.ends_with('?');
            let base = c.trim_end_matches('?').split(':').next().unwrap_or(c);
            (!optional && !self.capabilities.contains(base)).then(|| base.to_owned())
        });
        let network = m.launch.as_ref().is_some_and(|l| l.needs_network);
        let setup = m.fetch.as_ref().is_some_and(|f| f.enabled);
        let availability = if incompatible {
            Availability::IncompatibleRuntime {
                required: format!("{}@{}", m.runtime.family, m.runtime.abi),
                available: format!("{}@{}", self.runtime_family, self.runtime_abi),
            }
        } else if let Some(capability) = unsupported {
            Availability::UnsupportedCapability { capability }
        } else if setup {
            Availability::NeedsSetup {
                reason: m
                    .fetch
                    .as_ref()
                    .and_then(|f| f.reason.clone())
                    .unwrap_or_else(|| "setup required".into()),
            }
        } else if network {
            Availability::NeedsNetwork {
                reason: "network required".into(),
            }
        } else {
            Availability::Ready
        };
        let result = match &availability {
            Availability::IncompatibleRuntime { required, .. } => {
                ProviderItemResult::Incompatible {
                    item_id: id.clone(),
                    required: required.clone(),
                }
            }
            Availability::NeedsNetwork { .. } => ProviderItemResult::NetworkRequired {
                item_id: id.clone(),
            },
            Availability::NeedsSetup { .. } => ProviderItemResult::SetupRequired {
                item_id: id.clone(),
            },
            _ => ProviderItemResult::Valid {
                item_id: id.clone(),
            },
        };
        consume_reserved(&m);
        let digest = format!("{:x}", Sha256::digest(bytes));
        let requirements = m
            .app
            .capabilities
            .iter()
            .map(|c| Requirement {
                capability: c.trim_end_matches('?').into(),
                optional: c.ends_with('?'),
            })
            .collect();
        let variant = Variant {
            id: format!("{id}:{}", m.runtime.family),
            provider_id: "installed-applications".into(),
            availability,
            requirements,
            provenance: Provenance {
                provider_id: "installed-applications".into(),
                app_version: m.app.version,
                upstream_version: m.app.upstream_version,
                runtime_family: m.runtime.family,
                runtime_abi: m.runtime.abi,
                platform_version: m.runtime.platform_version,
            },
            launch_target: AppManifestRef {
                app_id: m.app.id.clone(),
                descriptor_path: path,
                observed_digest: digest,
            },
        };
        let icon_reference = m.app.icon;
        items.push(CatalogItem {
            id,
            title: m.app.name.unwrap_or(m.app.id),
            kind: m.app.category.unwrap_or(AppKind::Game),
            presentation: Presentation {
                icon_decodable: icon_reference.is_some(),
                icon_reference,
            },
            tags: vec![],
            variants: vec![variant],
        });
        results.push(result);
    }
}

fn invalid(path: PathBuf, message: String, kind: ManifestErrorKind) -> ProviderItemResult {
    ProviderItemResult::Invalid {
        descriptor_path: path,
        error: ManifestError { kind, message },
    }
}
fn validate(m: &Manifest) -> Result<(), ManifestError> {
    let mut v = vec![];
    if m.app.id.is_empty()
        || !m.app.id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
    {
        v.push("invalid app.id");
    }
    if !m.runtime.family.starts_with("pocketforge/") {
        v.push("invalid runtime.family");
    }
    if m.runtime.abi.is_empty() || !m.runtime.abi.bytes().all(|b| b.is_ascii_digit()) {
        v.push("invalid runtime.abi");
    }
    let mut seen = BTreeSet::new();
    for c in &m.app.capabilities {
        if !seen.insert(c) || c.is_empty() || c.bytes().any(|b| b.is_ascii_whitespace()) {
            v.push("invalid or duplicate capability");
        }
    }
    if m.launch.as_ref().is_some_and(|l| l.exec.is_empty()) {
        v.push("empty launch.exec");
    }
    if let Some(f) = &m.fetch {
        if f.enabled && f.reason.as_deref().unwrap_or_default().is_empty() {
            v.push("enabled fetch requires reason");
        }
        for x in f.files.as_deref().unwrap_or_default() {
            if x.sha256.len() != 64
                || !x
                    .sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            {
                v.push("invalid fetch sha256");
            }
        }
    }
    if v.is_empty() {
        Ok(())
    } else {
        Err(ManifestError {
            kind: ManifestErrorKind::Validation,
            message: v.join("; "),
        })
    }
}
fn consume_reserved(m: &Manifest) {
    let _ = m.app.order;
    if let Some(l) = &m.launch {
        let _ = (&l.exec, l.takes_display, l.audio);
    }
    if let Some(h) = &m.health {
        let _ = (&h.preflight, h.timeout_sec);
    }
    if let Some(f) = &m.fetch {
        let _ = &f.destination;
        for x in f.files.as_deref().unwrap_or_default() {
            let _ = (
                &x.url,
                x.size,
                &x.format,
                &x.dest,
                x.strip_components,
                &x.executable,
            );
        }
    }
}
fn result_key(r: &ProviderItemResult) -> String {
    match r {
        ProviderItemResult::Valid { item_id }
        | ProviderItemResult::Incompatible { item_id, .. }
        | ProviderItemResult::NetworkRequired { item_id }
        | ProviderItemResult::SetupRequired { item_id } => item_id.clone(),
        ProviderItemResult::Invalid {
            descriptor_path, ..
        } => descriptor_path.display().to_string(),
    }
}
