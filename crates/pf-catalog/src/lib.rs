//! Installed-application catalog with immutable snapshots and a separate favorites overlay.

use fs2::FileExt;
use pf_app_manifest::{
    AppCategory, Fetch, ManifestErrorKind as SharedManifestErrorKind, ReasonCode,
    fetch_receipt_key, parse_capability_requirement, parse_manifest,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
    /// Present for a fetch-enabled app that is not yet Ready; carries the typed
    /// setup intent plus receipt/result-derived state. `None` once Ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_setup: Option<FetchSetup>,
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
/// A typed, data-only request to run `pf-app-fetch@<id>`. W4 emits the intent;
/// W4b (tsp-ziac.4.3.8) wires it to the session authority. pf-catalog never
/// starts a unit itself.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FetchSetupIntent {
    pub app_id: String,
    pub unit: String,
}
/// The launcher-visible liveness/outcome of a `[fetch]` app's setup.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FetchSetupState {
    /// No run is live: the receipt is absent, stale or invalid, or a retry is due.
    NotStarted,
    /// `result.json` says running and a run holds `.pf-fetch/lock`.
    SettingUp,
    /// `result.json` says running but no run holds the lock (SIGKILL/OOM/power loss).
    Interrupted,
    /// The last run wrote `outcome = "failed"`.
    Failed,
}
/// The setup surface for one fetch-enabled variant, built from the W3 receipt and
/// `result.json`. `message` is the text the existing `Availability::NeedsSetup`
/// UI renders (the W0 class text for a failed run).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FetchSetup {
    pub intent: FetchSetupIntent,
    pub state: FetchSetupState,
    /// The W0 reason code for a failed or stale/invalid receipt.
    pub reason: Option<String>,
    /// The W0 reason-code class.
    pub class: Option<String>,
    /// The user-visible class text (or the descriptor's setup reason when idle).
    pub message: String,
    /// A retry is always offered for a non-live run, whatever the reason code.
    pub retry_offered: bool,
    pub run_id: Option<String>,
}
/// What the launcher can see of a setup run without the journal: whether a run
/// holds `.pf-fetch/lock`, and `result.json`'s outcome and run_id. W4b
/// (tsp-ziac.4.3.8, N6) polls this for one item after starting its setup unit.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FetchRunProbe {
    pub lock_held: bool,
    pub outcome: Option<String>,
    pub run_id: Option<String>,
}

/// Probe one app's `.pf-fetch/` state directory (see [`FetchRunProbe`]).
#[must_use]
pub fn probe_fetch_run(_state_dir: &Path) -> FetchRunProbe {
    FetchRunProbe::default()
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

/// Default `pf-app-fetch --state-dir` parent (image unit `pf-app-fetch@.service`).
pub const DEFAULT_FETCH_STATE_ROOT: &str = "/var/lib/pocketforge/apps";

pub struct InstalledAppProvider {
    root: PathBuf,
    /// Where `pf-app-fetch` keeps per-app state (`<state_root>/<app_id>/`).
    state_root: PathBuf,
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
            state_root: PathBuf::from(DEFAULT_FETCH_STATE_ROOT),
            favorites: favorites.into(),
            runtime_family: family.into(),
            runtime_abi: abi.into(),
            platform_version: None,
            capabilities: BTreeSet::new(),
            observed_at: 0,
        }
    }
    /// Sets the root holding per-app fetch state (receipt, `.pf-fetch/`, dests),
    /// separate from the read-only descriptor root.
    #[must_use]
    pub fn with_state_root(mut self, value: impl Into<PathBuf>) -> Self {
        self.state_root = value.into();
        self
    }
    /// Probe `app_id`'s setup run under the state root. An id that is not one
    /// plain path component is never joined and reads as "no run".
    #[must_use]
    pub fn probe_fetch_run(&self, _app_id: &str) -> FetchRunProbe {
        FetchRunProbe::default()
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
        let fetch_evaluation = m
            .fetch
            .as_ref()
            .filter(|fetch| fetch.enabled)
            .map(|fetch| evaluate_fetch(&self.state_root.join(&m.app.id), &m.app.id, fetch));
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
        } else {
            unsupported.map(|capability| {
                (
                    Availability::UnsupportedCapability {
                        capability: capability.clone(),
                    },
                    capability,
                    ReasonCode::UnsupportedCapability,
                )
            })
        };
        let availability = if let Some((availability, _, _)) = &incompatibility {
            availability.clone()
        } else if let Some(evaluation) = &fetch_evaluation {
            if evaluation.ready {
                Availability::Ready
            } else {
                Availability::NeedsSetup {
                    reason: evaluation.setup.message.clone(),
                }
            }
        } else {
            Availability::Ready
        };
        let fetch_setup = if matches!(availability, Availability::NeedsSetup { .. }) {
            fetch_evaluation
                .as_ref()
                .map(|evaluation| evaluation.setup.clone())
        } else {
            None
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
            fetch_setup,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ReceiptVerdict {
    Valid,
    Absent,
    Stale,
    Invalid,
}

struct FetchEvaluation {
    ready: bool,
    setup: FetchSetup,
}

/// Decides whether a fetch-enabled app is Ready and, when it is not, builds the
/// setup surface consumed by the UI. The receipt's embedded manifest is only
/// compared statistically (directory entries, file count, total bytes); no
/// payload hashing happens here.
fn evaluate_fetch(state_dir: &Path, app_id: &str, fetch: &Fetch) -> FetchEvaluation {
    let intent = FetchSetupIntent {
        app_id: app_id.to_owned(),
        unit: format!("pf-app-fetch@{app_id}.service"),
    };
    let verdict = check_fetch_receipt(state_dir, fetch);
    if verdict == ReceiptVerdict::Valid {
        return FetchEvaluation {
            ready: true,
            setup: FetchSetup {
                intent,
                state: FetchSetupState::NotStarted,
                reason: None,
                class: None,
                message: String::new(),
                retry_offered: false,
                run_id: None,
            },
        };
    }
    let result = read_fetch_result(state_dir);
    let outcome = result
        .as_ref()
        .and_then(|value| value.get("outcome"))
        .and_then(Value::as_str);
    let run_id = result
        .as_ref()
        .and_then(|value| value.get("run_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let setup = match outcome {
        Some("running") => running_setup(intent, run_id, fetch_lock_held(state_dir)),
        Some("failed") => failed_setup(intent, result.as_ref(), run_id),
        _ => pending_setup(intent, fetch, verdict, run_id),
    };
    FetchEvaluation {
        ready: false,
        setup,
    }
}

fn running_setup(intent: FetchSetupIntent, run_id: Option<String>, lock_held: bool) -> FetchSetup {
    FetchSetup {
        intent,
        state: if lock_held {
            FetchSetupState::SettingUp
        } else {
            FetchSetupState::Interrupted
        },
        reason: None,
        class: None,
        message: if lock_held {
            "Setting up…".into()
        } else {
            "Setup was interrupted.".into()
        },
        retry_offered: !lock_held,
        run_id,
    }
}

fn failed_setup(
    intent: FetchSetupIntent,
    result: Option<&Value>,
    run_id: Option<String>,
) -> FetchSetup {
    let reason = result
        .and_then(|value| value.get("reason"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let class = reason
        .as_deref()
        .and_then(fetch_reason_class)
        .map(str::to_owned)
        .or_else(|| {
            result
                .and_then(|value| value.get("class"))
                .and_then(Value::as_str)
                .filter(|class| fetch_class_text(class).is_some())
                .map(str::to_owned)
        });
    let message = class
        .as_deref()
        .and_then(fetch_class_text)
        .unwrap_or("Setup failed. Try again.")
        .to_owned();
    FetchSetup {
        intent,
        state: FetchSetupState::Failed,
        reason,
        class,
        message,
        retry_offered: true,
        run_id,
    }
}

fn pending_setup(
    intent: FetchSetupIntent,
    fetch: &Fetch,
    verdict: ReceiptVerdict,
    run_id: Option<String>,
) -> FetchSetup {
    let (reason, class, message) = match verdict {
        ReceiptVerdict::Stale => (
            Some("receipt_stale".into()),
            Some("setup_required".into()),
            fetch_class_text("setup_required")
                .unwrap_or_default()
                .into(),
        ),
        ReceiptVerdict::Invalid => (
            Some("state_invalid".into()),
            Some("setup_required".into()),
            fetch_class_text("setup_required")
                .unwrap_or_default()
                .into(),
        ),
        ReceiptVerdict::Absent | ReceiptVerdict::Valid => (
            None,
            None,
            fetch
                .reason
                .clone()
                .unwrap_or_else(|| "setup required".into()),
        ),
    };
    FetchSetup {
        intent,
        state: FetchSetupState::NotStarted,
        reason,
        class,
        message,
        retry_offered: true,
        run_id,
    }
}

fn check_fetch_receipt(state_dir: &Path, fetch: &Fetch) -> ReceiptVerdict {
    let path = state_dir.join(".pf-fetch").join("receipt.json");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return ReceiptVerdict::Absent;
        }
        Err(_) => return ReceiptVerdict::Invalid,
    };
    let receipt: Value = match serde_json::from_str(&text) {
        Ok(receipt) => receipt,
        Err(_) => return ReceiptVerdict::Invalid,
    };
    if receipt.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return ReceiptVerdict::Invalid;
    }
    let Some(expected_key) = fetch_receipt_key(fetch) else {
        return ReceiptVerdict::Invalid;
    };
    let Some(recorded_key) = receipt.get("key").and_then(Value::as_str) else {
        return ReceiptVerdict::Invalid;
    };
    if recorded_key != expected_key.as_str() {
        return ReceiptVerdict::Stale;
    }
    let Some(files) = receipt.get("files").and_then(Value::as_array) else {
        return ReceiptVerdict::Invalid;
    };
    if files.is_empty() || !files.iter().all(|file| payload_matches(state_dir, file)) {
        return ReceiptVerdict::Invalid;
    }
    // The receipt must cover exactly the descriptor's declared dests.
    let mut recorded: Vec<&str> = files
        .iter()
        .filter_map(|file| file.get("dest").and_then(Value::as_str))
        .collect();
    let mut declared: Vec<&str> = fetch
        .files
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|file| file.dest.as_deref())
        .collect();
    recorded.sort_unstable();
    declared.sort_unstable();
    if recorded != declared {
        return ReceiptVerdict::Invalid;
    }
    ReceiptVerdict::Valid
}

fn payload_matches(state_dir: &Path, file: &Value) -> bool {
    let Some(dest) = file.get("dest").and_then(Value::as_str) else {
        return false;
    };
    if !is_safe_receipt_dest(dest) {
        return false;
    }
    let Some(file_count) = file.get("file_count").and_then(Value::as_u64) else {
        return false;
    };
    let Some(total_bytes) = file.get("total_bytes").and_then(Value::as_u64) else {
        return false;
    };
    let Some(manifest) = file.get("manifest").and_then(Value::as_array) else {
        return false;
    };
    if u64::try_from(manifest.len()).ok() != Some(file_count) {
        return false;
    }
    let mut recorded_bytes = 0_u64;
    for entry in manifest {
        let Some(size) = entry.get("size").and_then(Value::as_u64) else {
            return false;
        };
        if entry.get("path").and_then(Value::as_str).is_none()
            || entry.get("sha256").and_then(Value::as_str).is_none()
        {
            return false;
        }
        recorded_bytes = recorded_bytes.saturating_add(size);
    }
    if recorded_bytes != total_bytes {
        return false;
    }
    let mut observed_count = 0_u64;
    let mut observed_bytes = 0_u64;
    stat_directory(
        &state_dir.join(dest),
        &mut observed_count,
        &mut observed_bytes,
    ) && observed_count == file_count
        && observed_bytes == total_bytes
}

/// A receipt dest must be a single normal component: `pf-app-fetch` refuses
/// absolute paths, `..`, and its own `.`-prefixed state directory.
fn is_safe_receipt_dest(dest: &str) -> bool {
    !dest.is_empty()
        && !dest.starts_with('.')
        && !dest.contains('/')
        && !matches!(dest, "config" | "state" | "cache")
}

/// Stat-only recursive walk. Symlinks and non-regular files fail the walk:
/// the receipt promises a fully materialized payload, not a link farm.
fn stat_directory(dir: &Path, files: &mut u64, bytes: &mut u64) -> bool {
    let Ok(metadata) = fs::symlink_metadata(dir) else {
        return false;
    };
    if !metadata.is_dir() {
        return false;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return false;
    };
    for entry in entries {
        let Ok(entry) = entry else {
            return false;
        };
        let Ok(metadata) = fs::symlink_metadata(entry.path()) else {
            return false;
        };
        if metadata.is_dir() {
            if !stat_directory(&entry.path(), files, bytes) {
                return false;
            }
        } else if metadata.is_file() {
            *files += 1;
            *bytes = bytes.saturating_add(metadata.len());
        } else {
            return false;
        }
    }
    true
}

fn read_fetch_result(state_dir: &Path) -> Option<Value> {
    let text = fs::read_to_string(state_dir.join(".pf-fetch").join("result.json")).ok()?;
    serde_json::from_str(&text).ok()
}

/// Non-blocking flock probe. The lock file's content is never read: only
/// whether a live pf-app-fetch run holds the flock matters.
fn fetch_lock_held(state_dir: &Path) -> bool {
    let path = state_dir.join(".pf-fetch").join("lock");
    // Only WouldBlock proves a live run owns the flock. Any open or lock error
    // is "not held", which yields Interrupted plus a retry rather than a
    // "setting up" that can never clear.
    let Ok(file) = fs::File::open(&path) else {
        return false;
    };
    matches!(
        file.try_lock_exclusive(),
        Err(ref error) if error.kind() == std::io::ErrorKind::WouldBlock
    )
}

fn fetch_reason_class(reason: &str) -> Option<&'static str> {
    match reason {
        "offline" | "download_truncated" => Some("network"),
        "http_error" => Some("server"),
        "http_refused" => Some("source_gone"),
        "redirect_refused" => Some("source_moved"),
        "download_too_large"
        | "size_mismatch"
        | "digest_mismatch"
        | "signature_invalid"
        | "version_invalid"
        | "version_below_floor"
        | "version_mismatch" => Some("source_mismatch"),
        "archive_invalid"
        | "archive_member_unsafe"
        | "descriptor_invalid"
        | "trust_key_invalid" => Some("app_defect"),
        "disk_full" => Some("storage"),
        "cancelled" => Some("cancelled"),
        "receipt_stale" | "state_invalid" => Some("setup_required"),
        "abi_mismatch" => Some("unsupported"),
        _ => None,
    }
}

fn fetch_class_text(class: &str) -> Option<&'static str> {
    match class {
        "network" => {
            Some("Can't reach the download server. Check the Wi-Fi connection and try again.")
        }
        "server" => Some("The download server refused the request. Try again later."),
        "source_mismatch" => Some(
            "The download didn't match what this app expects. If you're on public Wi-Fi, sign in to it and try again; otherwise the app needs an update.",
        ),
        "source_gone" => {
            Some("The download is no longer available from its source. The app needs an update.")
        }
        "source_moved" => Some(
            "The download moved to a server this system doesn't allow yet. A system update is needed.",
        ),
        "app_defect" => Some("This app's download setup is broken. The app needs an update."),
        "storage" => Some("Not enough storage space. Free some space and try again."),
        "cancelled" => Some("Setup was cancelled."),
        "setup_required" => Some("This app needs to finish setup."),
        "unsupported" => Some("This app needs a newer system version."),
        _ => None,
    }
}
