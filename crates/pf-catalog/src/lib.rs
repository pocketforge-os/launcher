//! Installed-application catalog with immutable snapshots and a separate favorites overlay.

use fs2::FileExt;
use pf_app_manifest::{
    AppCategory, ManifestErrorKind as SharedManifestErrorKind, ReasonCode,
    parse_capability_requirement, parse_manifest,
};
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
    #[serde(default)]
    pub needs_network: bool,
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
        #[serde(default)]
        reason: String,
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

pub struct InstalledAppProvider {
    root: PathBuf,
    favorites: PathBuf,
    runtime_family: String,
    runtime_abi: String,
    platform_version: Option<String>,
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
            platform_version: None,
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
    pub fn with_platform_version(mut self, value: Option<String>) -> Self {
        self.platform_version = value;
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
        let m = match parse_manifest(text) {
            Ok(v) => v,
            Err(e) => {
                let kind = match e.kind {
                    SharedManifestErrorKind::Parse => ManifestErrorKind::Parse,
                    SharedManifestErrorKind::Invalid
                    | SharedManifestErrorKind::InvalidLaunchExec => ManifestErrorKind::Validation,
                };
                results.push(invalid(path, e.to_string(), kind));
                return;
            }
        };
        let id = format!("installed-applications:{}", m.app.id);
        let unsupported = m.app.capabilities.iter().find_map(|c| {
            let requirement = parse_capability_requirement(c);
            (!requirement.optional
                && requirement.base != "egress"
                && !self.capabilities.contains(&requirement.base))
            .then_some(requirement.base)
        });
        let setup = m.fetch.as_ref().is_some_and(|f| f.enabled);
        let incompatibility = if m.runtime.family != self.runtime_family {
            Some((
                Availability::IncompatibleRuntime {
                    required: format!("{}@{}", m.runtime.family, m.runtime.abi),
                    available: format!("{}@{}", self.runtime_family, self.runtime_abi),
                },
                m.runtime.family.clone(),
                ReasonCode::RuntimeFamilyMismatch,
            ))
        } else if m.runtime.abi != self.runtime_abi {
            Some((
                Availability::IncompatibleRuntime {
                    required: format!("{}@{}", m.runtime.family, m.runtime.abi),
                    available: format!("{}@{}", self.runtime_family, self.runtime_abi),
                },
                m.runtime.abi.clone(),
                ReasonCode::RuntimeAbiMismatch,
            ))
        } else if m
            .runtime
            .platform_version
            .as_deref()
            .is_some_and(|version| self.platform_version.as_deref() != Some(version))
        {
            let required = m.runtime.platform_version.clone().unwrap_or_default();
            Some((
                Availability::IncompatibleRuntime {
                    required: format!("platform {required}"),
                    available: self.platform_version.as_ref().map_or_else(
                        || "platform unspecified".into(),
                        |version| format!("platform {version}"),
                    ),
                },
                required,
                ReasonCode::PlatformVersionMismatch,
            ))
        } else if let Some(capability) = unsupported {
            Some((
                Availability::UnsupportedCapability {
                    capability: capability.clone(),
                },
                capability,
                ReasonCode::UnsupportedCapability,
            ))
        } else {
            None
        };
        let availability = if let Some((availability, _, _)) = &incompatibility {
            availability.clone()
        } else if setup {
            Availability::NeedsSetup {
                reason: m
                    .fetch
                    .as_ref()
                    .and_then(|f| f.reason.clone())
                    .unwrap_or_else(|| "setup required".into()),
            }
        } else {
            Availability::Ready
        };
        let result = if let Some((_, required, reason)) = incompatibility {
            ProviderItemResult::Incompatible {
                item_id: id.clone(),
                required,
                reason: reason.as_str().into(),
            }
        } else if matches!(availability, Availability::NeedsSetup { .. }) {
            ProviderItemResult::SetupRequired {
                item_id: id.clone(),
            }
        } else {
            ProviderItemResult::Valid {
                item_id: id.clone(),
            }
        };
        let digest = format!("{:x}", Sha256::digest(bytes));
        let requirements = m
            .app
            .capabilities
            .iter()
            .map(|capability| {
                let requirement = parse_capability_requirement(capability);
                Requirement {
                    capability: requirement.modifier.as_ref().map_or_else(
                        || requirement.base.clone(),
                        |modifier| format!("{}:{modifier}", requirement.base),
                    ),
                    optional: requirement.optional,
                }
            })
            .collect();
        let needs_network = m.launch.as_ref().is_some_and(|launch| launch.needs_network);
        let variant = Variant {
            id: format!("{id}:{}", m.runtime.family),
            provider_id: "installed-applications".into(),
            availability,
            needs_network,
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
            kind: app_kind(m.app.category),
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
fn app_kind(category: Option<AppCategory>) -> AppKind {
    match category.unwrap_or(AppCategory::Game) {
        AppCategory::Media => AppKind::Media,
        AppCategory::Stream => AppKind::Stream,
        AppCategory::Game => AppKind::Game,
        AppCategory::System => AppKind::System,
        AppCategory::Settings => AppKind::Settings,
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
