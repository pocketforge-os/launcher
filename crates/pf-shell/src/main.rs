use pf_catalog::{
    CatalogItem, CatalogRevision, CatalogSnapshot, FavoriteCommitResult, InstalledAppProvider,
    VariantPinCommitResult,
};
use pf_framehost::{FbdevHost, OffscreenHost};
#[cfg(feature = "wayland")]
use pf_framehost_wayland::{Key, KeyEvent, KeyState, RepeatInfo, WaylandHost};
use pf_input_map::{DeviceContract, EffectiveMap, JsonRemapStore, MemoryStore, RemapStore};
use pf_ports::{
    ActionEvent, ActionPoll, AppliedNetworkEnabled, AppliedTransferState, AppliedValue,
    ChangeAuthority, Deadline, EffectivePreference, FakeNetworkPort, FakePowerPort,
    FakePreferencePort, FakeTimePort, FakeTransferPort, FrameHost, IdlePolicy, LaunchResult,
    MonotonicTime, NetworkError, NetworkPort, NetworkState, NtpState, ObservedSessionState,
    PowerAction, PowerCapability, PowerError, PowerPort, PowerRequestResult, PreferenceChange,
    PreferenceChangeResult, PreferenceError, PreferenceKey, PreferencePoll, PreferencePort,
    PreferenceValue, SessionError, SessionEvent, SessionPoll, SessionPort, ShellAction, Support,
    TerminalReceipt, TimeCapabilities, TimeError, TimePort, TimeState, TransferError, TransferPort,
    TransferService, TransferServiceState, WifiCredential, WifiNetwork, WifiSecurity,
};
use pf_prefs::PrefsStore;
use pf_prefs_port::PrefsPreferencePort;
use pf_render::{RasterFrame, RenderNote};
use pf_scene::{Insets, Node, Orientation, Role, SurfaceMetrics};
use pf_session_authority::{EndPrecision, EndStamp, HistoryEntry};
use pf_session_client::{SessionClient, SocketTransport};
use pf_shell::{
    EvdevActionSource, EvdevInputEvent, FavoriteCatalog, GamepadRemap, commit_favorite,
    commit_pinned_variant, control_bindings, favorite_footer_prompt, footer_prompt,
    safe_return_options,
};
use pf_shell_core::{Effect, ShellCore};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::{
    collections::VecDeque,
    env, fs,
    io::{BufWriter, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const DEFAULT_SESSION_SOCKET: &str = "/run/pocketforge/session-authority.sock";
const MAX_CATALOG_ART_BYTES: u64 = 8 * 1024 * 1024;
const RUNTIME_FAMILY: &str = "pocketforge/native";
const RUNTIME_ABI: &str = "1";
// A future input-repeat preference may own these handheld defaults.
const EVDEV_REPEAT_DELAY: Duration = Duration::from_millis(400);
const EVDEV_REPEAT_INTERVAL: Duration = Duration::from_millis(80);
const HELP: &str = "pf-shell modes:\n  --wayland                 interactive desktop window\n  --fbdev                   interactive framebuffer\n  --catalog-root <dir>      scan installed app manifests\n  --catalog-snapshot <file> load an exact, read-only CatalogSnapshot JSON; relative art paths resolve beside the snapshot (conflicts with --catalog-root)\n  --desktop-sim-script      headless launch/return proof against session authority\n  --desktop-sim-supervise   observe desktop-sim marker lifecycle\n  --sim-frame               write one framebuffer fixture\n  --settings-evidence       write fixture PNGs\n\nWayland keyboard (only actions present in the effective input map are enabled):\n  Arrows   Move focus\n  [, PageUp / ], PageDown   Previous / next room\n  Enter    Activate\n  Space    Start / continue\n  Escape, Backspace  Back\n  Tab, F   Quick / toggle favorite\n  S        Safe return\n";

fn empty_catalog_snapshot() -> Result<CatalogSnapshot, String> {
    let mut snapshot: CatalogSnapshot =
        serde_json::from_str(include_str!("../fixtures/catalog.json"))
            .map_err(|e| e.to_string())?;
    snapshot.items.clear();
    snapshot.provider_results.clear();
    snapshot.user_projection.favorite_item_ids.clear();
    snapshot.user_projection.pinned_variant_ids.clear();
    Ok(snapshot)
}

fn catalog_snapshot(
    provider: &InstalledAppProvider,
    root: &Path,
) -> Result<CatalogSnapshot, String> {
    match provider.snapshot() {
        Ok(snapshot) => Ok(snapshot),
        Err(pf_catalog::ProviderError::Io(error))
            if error.kind() == std::io::ErrorKind::NotFound && !root.exists() =>
        {
            eprintln!(
                "pf-shell: catalog root {} is missing; continuing with an empty catalog",
                root.display()
            );
            empty_catalog_snapshot()
        }
        Err(error) => Err(format!("catalog: {error:?}")),
    }
}

fn installed_app_provider(root: &Path, favorites_path: PathBuf) -> InstalledAppProvider {
    InstalledAppProvider::new(root, favorites_path, RUNTIME_FAMILY, RUNTIME_ABI)
}

#[cfg(feature = "wayland")]
fn effective_keyboard_action(map: &EffectiveMap, key: Key, keysym: u32) -> Option<ShellAction> {
    let action = match (key, keysym) {
        (Key::Up, _) => "Move.up",
        (Key::Down, _) => "Move.down",
        (Key::Left, _) => "Move.left",
        (Key::Right, _) => "Move.right",
        (Key::Char('['), _) | (_, 0xff55) => "Room.previous",
        (Key::Char(']'), _) | (_, 0xff56) => "Room.next",
        (Key::Enter, _) => "Activate",
        (_, 0x20) => "Start",
        (Key::Escape, _) | (_, 0xff08) => "Back",
        (_, 0xff09) | (Key::Char('f' | 'F'), _) => "Quick",
        (Key::Char('s' | 'S'), _) => "SafeReturn",
        _ => return None,
    };
    (action.starts_with("Room.")
        || map
            .mappings()
            .iter()
            .any(|mapping| mapping.action == action))
    .then(|| match action {
        "Move.up" => ShellAction::Move(pf_scene::AxisMove::Up),
        "Move.down" => ShellAction::Move(pf_scene::AxisMove::Down),
        "Move.left" => ShellAction::Move(pf_scene::AxisMove::Left),
        "Move.right" => ShellAction::Move(pf_scene::AxisMove::Right),
        "Room.previous" | "Room.next" => ShellAction::Custom(action.into()),
        "Activate" => ShellAction::Activate,
        "Start" => ShellAction::Custom("Start".into()),
        "Back" => ShellAction::Back,
        "Quick" => ShellAction::Custom("Favorite".into()),
        "SafeReturn" => ShellAction::Custom("SafeReturn".into()),
        _ => unreachable!("keyboard action table is exhaustive"),
    })
}

#[derive(Default)]
struct KeyRepeatScheduler {
    held: BTreeMap<u32, (ShellAction, Duration)>,
}

impl KeyRepeatScheduler {
    fn transition(
        &mut self,
        code: u32,
        pressed: bool,
        action: Option<ShellAction>,
        now: Duration,
        delay: Duration,
    ) {
        if !pressed {
            self.held.remove(&code);
        } else if let Some(action @ ShellAction::Move(_)) = action {
            self.held.insert(code, (action, now + delay));
        }
    }

    fn due(&mut self, now: Duration, interval: Duration) -> Vec<ShellAction> {
        if interval.is_zero() {
            return Vec::new();
        }
        let mut due = Vec::new();
        for (action, next) in self.held.values_mut() {
            while *next <= now {
                due.push(action.clone());
                *next += interval;
            }
        }
        due
    }

    fn clear(&mut self) {
        self.held.clear();
    }
}

fn vendored_art(reference: &str) -> Option<Arc<[u8]>> {
    match reference {
        "fixture-art:ridgeline.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/ridgeline.png")[..],
        )),
        "fixture-art:hollow-tides.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/hollow-tides.png")[..],
        )),
        "fixture-art:sunwake.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/sunwake.png")[..],
        )),
        "fixture-art:moth-and-lantern.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/moth-and-lantern.png")[..],
        )),
        "fixture-art:bellwether.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/bellwether.png")[..],
        )),
        "fixture-art:torchbug.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/torchbug.png")[..],
        )),
        "fixture-art:northlight.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/northlight.png")[..],
        )),
        "fixture-art:petrichor.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/petrichor.png")[..],
        )),
        "fixture-art:lumen-vale.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/lumen-vale.png")[..],
        )),
        "fixture-art:redshift-alley.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/redshift-alley.png")[..],
        )),
        "fixture-art:quiet-machines.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/quiet-machines.png")[..],
        )),
        "fixture-art:low-orbit.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/low-orbit.png")[..],
        )),
        "fixture-art:paper-armada.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/paper-armada.png")[..],
        )),
        "fixture-art:vega-crossing.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/vega-crossing.png")[..],
        )),
        "fixture-art:iron-meridian.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/iron-meridian.png")[..],
        )),
        "fixture-art:signal-decay.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/signal-decay.png")[..],
        )),
        "fixture-art:milewide.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/milewide.png")[..],
        )),
        "fixture-art:orchard-of-glass.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/orchard-of-glass.png")[..],
        )),
        "fixture-art:cinder-loop.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/cinder-loop.png")[..],
        )),
        "fixture-art:halfmoon-harbor.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/halfmoon-harbor.png")[..],
        )),
        "fixture-art:fern-and-fathom.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/fern-and-fathom.png")[..],
        )),
        "fixture-art:corrupt.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/corrupt.png")[..],
        )),
        _ => None,
    }
}

fn fixture_core(snapshot: &CatalogSnapshot, theme: &pf_theme::Theme, reduced: bool) -> ShellCore {
    ShellCore::boot_with_art(snapshot, theme, reduced, |_, reference| {
        vendored_art(reference)
    })
}

enum ArtBase<'a> {
    DescriptorDirectory,
    SnapshotDirectory(&'a Path),
}

fn art_base_path<'a>(item: &'a CatalogItem, policy: &ArtBase<'a>) -> Option<&'a Path> {
    match policy {
        ArtBase::DescriptorDirectory => item
            .variants
            .first()?
            .launch_target
            .descriptor_path
            .parent(),
        // Snapshot paths can describe another machine, so their relative art references
        // deliberately use the portable snapshot bundle's directory instead.
        ArtBase::SnapshotDirectory(directory) => Some(directory),
    }
}

fn read_catalog_art(manifest_dir: &Path, reference: &str) -> Option<Arc<[u8]>> {
    let path = manifest_dir.join(reference);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.len() > MAX_CATALOG_ART_BYTES {
        eprintln!(
            "catalog art ignored: {} is {} bytes (limit {})",
            path.display(),
            metadata.len(),
            MAX_CATALOG_ART_BYTES
        );
        return None;
    }
    fs::read(path).ok().map(Arc::from)
}

fn manifest_art_core(
    snapshot: &CatalogSnapshot,
    theme: &pf_theme::Theme,
    reduced: bool,
    policy: ArtBase<'_>,
) -> ShellCore {
    ShellCore::boot_with_art(snapshot, theme, reduced, move |item, reference| {
        read_catalog_art(art_base_path(item, &policy)?, reference)
    })
}

fn catalog_core(snapshot: &CatalogSnapshot, theme: &pf_theme::Theme, reduced: bool) -> ShellCore {
    manifest_art_core(snapshot, theme, reduced, ArtBase::DescriptorDirectory)
}

fn snapshot_core(
    snapshot: &CatalogSnapshot,
    snapshot_path: &Path,
    theme: &pf_theme::Theme,
    reduced: bool,
) -> ShellCore {
    let directory = snapshot_path.parent().unwrap_or_else(|| Path::new(""));
    manifest_art_core(
        snapshot,
        theme,
        reduced,
        ArtBase::SnapshotDirectory(directory),
    )
}
struct SnapshotCatalog(CatalogSnapshot);

impl FavoriteCatalog for SnapshotCatalog {
    fn snapshot(&self) -> Result<CatalogSnapshot, String> {
        Ok(self.0.clone())
    }

    fn set_favorite(
        &self,
        _id: &str,
        _value: bool,
        _expected: CatalogRevision,
    ) -> Result<FavoriteCommitResult, String> {
        Err("catalog snapshots are read-only".into())
    }

    fn set_pinned_variant(
        &self,
        _item_id: &str,
        _variant_id: Option<&str>,
        _expected: CatalogRevision,
    ) -> Result<VariantPinCommitResult, String> {
        Err("catalog snapshots are read-only".into())
    }
}

fn load_catalog_snapshot(path: &Path) -> Result<CatalogSnapshot, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("catalog snapshot {}: {error}", path.display()))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("catalog snapshot {}: {error}", path.display()))
}

fn fixture_device_ports() -> (FakeNetworkPort, FakeTimePort, FakeTransferPort) {
    let mut network = FakeNetworkPort::new(NetworkState {
        interface_present: true,
        enabled: true,
        connected_ssid: Some("Moonlit Arcade".into()),
        signal: Some(78),
    });
    network.script_scan(Ok(vec![
        WifiNetwork {
            ssid: "Moonlit Arcade".into(),
            security: WifiSecurity::Personal,
            strength: 78,
        },
        WifiNetwork {
            ssid: "Cedar Workshop".into(),
            security: WifiSecurity::Personal,
            strength: 54,
        },
        WifiNetwork {
            ssid: "Open Lantern".into(),
            security: WifiSecurity::Open,
            strength: 31,
        },
    ]));
    let time = FakeTimePort::new(
        TimeCapabilities {
            manual_set_time: Support::Supported,
        },
        TimeState {
            wall_clock: std::time::SystemTime::UNIX_EPOCH,
            timezone: "UTC".into(),
            ntp_state: NtpState::Inactive,
        },
    );
    let transfer = FakeTransferPort::new(vec![
        TransferServiceState {
            service: TransferService::Sftp,
            support: Support::Supported,
            enabled: false,
            endpoint_info: Some("Available on the local network".into()),
        },
        TransferServiceState {
            service: TransferService::UsbMassStorage,
            support: Support::Unsupported,
            enabled: false,
            endpoint_info: None,
        },
    ]);
    (network, time, transfer)
}

fn load_durable_map_or_shipped(
    contract: DeviceContract,
    path: &Path,
) -> Result<EffectiveMap, String> {
    match EffectiveMap::load(contract.clone(), &JsonRemapStore::at(path)) {
        Ok(map) => Ok(map),
        Err(error) => {
            eprintln!(
                "pf-shell: remap store {} could not be loaded ({error:?}); using shipped controls",
                path.display()
            );
            let shipped = EffectiveMap::load(contract, &MemoryStore::default())
                .map_err(|fallback| format!("shipped input map: {fallback:?}"))?;
            let digest = Sha256::digest(fs::read(path).unwrap_or_default());
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("remaps.json");
            let quarantine =
                path.with_file_name(format!("{file_name}.corrupt-{}", hex(&digest[..8])));
            fs::rename(path, &quarantine).map_err(|quarantine_error| {
                format!(
                    "quarantine remap store {} as {}: {quarantine_error}",
                    path.display(),
                    quarantine.display()
                )
            })?;
            JsonRemapStore::at(path)
                .save(shipped.device_id(), shipped.mappings())
                .map_err(|save_error| {
                    format!("recover remap store {}: {save_error:?}", path.display())
                })?;
            Ok(shipped)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        print!("{HELP}");
        return Ok(());
    }
    validate_args(&args)?;
    let interactive_mode = args
        .iter()
        .any(|a| matches!(a.as_str(), "--fbdev" | "--wayland"));
    let fixture_mode = args.iter().any(|a| {
        matches!(
            a.as_str(),
            "--sim-frame" | "--settings-evidence" | "--desktop-sim-script"
        )
    }) || !interactive_mode;
    let state_dir = PathBuf::from(value(&args, "--state-dir").unwrap_or("./state"));
    let catalog_root =
        PathBuf::from(value(&args, "--catalog-root").unwrap_or("/opt/pocketforge/apps"));
    let snapshot_path = value(&args, "--catalog-snapshot").map(PathBuf::from);
    let installed = (!fixture_mode && snapshot_path.is_none())
        .then(|| installed_app_provider(&catalog_root, state_dir.join("favorites.json")));
    let snapshot: CatalogSnapshot = if let Some(path) = &snapshot_path {
        load_catalog_snapshot(path)?
    } else if let Some(provider) = &installed {
        catalog_snapshot(provider, &catalog_root)?
    } else {
        serde_json::from_str(include_str!("../fixtures/catalog.json")).map_err(|e| e.to_string())?
    };
    let snapshot_catalog = snapshot_path
        .as_ref()
        .map(|_| SnapshotCatalog(snapshot.clone()));
    let catalog: Option<&dyn FavoriteCatalog> = installed
        .as_ref()
        .map(|provider| provider as &dyn FavoriteCatalog)
        .or_else(|| {
            snapshot_catalog
                .as_ref()
                .map(|provider| provider as &dyn FavoriteCatalog)
        });
    let theme = pf_theme::flagship();
    let reduced = env::var_os("PF_REDUCE_MOTION").is_some();
    let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json"))
        .map_err(|e| format!("{e:?}"))?;
    let options = safe_return_options(&contract);
    let remap_path = state_dir.join("remaps.json");
    let glyphs = if fixture_mode {
        EffectiveMap::load(contract.clone(), &MemoryStore::default())
            .map_err(|e| format!("{e:?}"))?
    } else {
        load_durable_map_or_shipped(contract.clone(), &remap_path)?
    };
    let mut footer = footer_prompt(&glyphs);
    if let Some(hint) = favorite_footer_prompt(&glyphs, false) {
        if let Some(glyph) = hint.strip_suffix("  Favorite") {
            footer.push('\u{1f}');
            footer.push_str(glyph);
        }
    }
    let mut core = if fixture_mode {
        fixture_core(&snapshot, &theme, reduced)
    } else if let Some(path) = &snapshot_path {
        snapshot_core(&snapshot, path, &theme, reduced)
    } else {
        catalog_core(&snapshot, &theme, reduced)
    };
    core.set_control_bindings(control_bindings(&glyphs));
    core.authority_snapshot(false);
    if args.iter().any(|arg| arg == "--session-unavailable") {
        core.session_backend_unavailable_at_boot();
    }
    core.set_safe_return_options(options.iter().map(|(_, label)| label.clone()));
    let mut durable;
    let mut fixture;
    let preferences: &mut dyn PreferencePort;
    let first_run_complete;
    if fixture_mode {
        fixture = fixture_preferences();
        preferences = &mut fixture;
        first_run_complete = !args.iter().any(|a| a == "--first-run");
    } else {
        durable = DurablePreferences::open(&state_dir)?;
        first_run_complete = durable.first_run_complete()?;
        if let Some(label) = durable.safe_return_binding()? {
            core.set_safe_return_binding(label);
        }
        preferences = &mut durable;
    }
    core.load_preferences(preferences, first_run_complete)
        .map_err(|e| format!("preferences: {e:?}"))?;
    let mut fake_power;
    let mut unavailable_power;
    let power: &mut dyn PowerPort = if fixture_mode {
        fake_power = FakePowerPort::new(
            vec![
                PowerCapability {
                    action: PowerAction::PowerOff,
                    support: Support::Supported,
                },
                PowerCapability {
                    action: PowerAction::Restart,
                    support: Support::Supported,
                },
                PowerCapability {
                    action: PowerAction::Sleep,
                    support: Support::Unsupported,
                },
            ],
            IdlePolicy::default(),
        );
        &mut fake_power
    } else {
        unavailable_power = UnavailablePowerPort;
        &mut unavailable_power
    };
    core.load_power(power);
    let mut fake_network;
    let mut fake_time;
    let mut fake_transfer;
    let mut unavailable_network;
    let mut unavailable_time;
    let mut unavailable_transfer;
    let (network, time, transfer): (
        &mut dyn NetworkPort,
        &mut dyn TimePort,
        &mut dyn TransferPort,
    ) = if fixture_mode {
        (fake_network, fake_time, fake_transfer) = fixture_device_ports();
        (&mut fake_network, &mut fake_time, &mut fake_transfer)
    } else {
        unavailable_network = UnavailableNetworkPort;
        unavailable_time = UnavailableTimePort;
        unavailable_transfer = UnavailableTransferPort;
        (
            &mut unavailable_network,
            &mut unavailable_time,
            &mut unavailable_transfer,
        )
    };
    core.load_network(&mut *network);
    core.load_system(&*time, &*transfer);
    if args.iter().any(|a| a == "--sim-frame") {
        let path = value(&args, "--device").map_or_else(
            || env::var("PF_FB0").unwrap_or_else(|_| "/dev/fb0".into()),
            str::to_owned,
        );
        return emit_sim_frame(&mut core, &footer, Path::new(&path));
    }
    if args.iter().any(|a| a == "--fbdev") {
        let framebuffer = value(&args, "--device").unwrap_or("/dev/fb0");
        let input = value(&args, "--input").unwrap_or("/dev/input/event0");
        let mut host = FbdevHost::open(framebuffer).map_err(|e| e.to_string())?;
        let (mut actions, _) = EvdevActionSource::open_with_map(input, &contract, glyphs.clone())
            .map_err(|e| format!("input adapter: {e:?}"))?;
        let session_socket = value(&args, "--session-socket").unwrap_or(DEFAULT_SESSION_SOCKET);
        let mut input = EvdevInteractiveInput::new(&mut actions);
        return run_interactive(
            &mut host,
            &mut input,
            &mut core,
            footer,
            preferences,
            power,
            glyphs,
            catalog.expect("fbdev catalog"),
            Path::new(session_socket),
            network,
            time,
            transfer,
            &state_dir,
            JsonRemapStore::at(remap_path),
        );
    }
    #[cfg(feature = "wayland")]
    if args.iter().any(|a| a == "--wayland") {
        let mut host = WaylandHost::connect_with_size(1280, 720).map_err(|e| e.to_string())?;
        let mut input = WaylandInteractiveInput::new(glyphs.clone());
        let session_socket = value(&args, "--session-socket").unwrap_or(DEFAULT_SESSION_SOCKET);
        return run_interactive(
            &mut host,
            &mut input,
            &mut core,
            footer,
            preferences,
            power,
            glyphs,
            catalog.expect("wayland catalog"),
            Path::new(session_socket),
            network,
            time,
            transfer,
            &state_dir,
            JsonRemapStore::at(remap_path),
        );
    }
    let out = Path::new(value(&args, "--out").unwrap_or("evidence/offscreen"));
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let metrics = SurfaceMetrics {
        logical_width: 1280.0,
        logical_height: 720.0,
        scale: 1.0,
        safe_insets: Insets::default(),
        orientation: Orientation::Landscape,
    };
    let mut host = OffscreenHost::new(metrics);
    apply_text_scale(&mut host, &core)?;
    if args.iter().any(|a| a == "--desktop-sim-script") {
        let session_socket = value(&args, "--session-socket").unwrap_or(DEFAULT_SESSION_SOCKET);
        let authority_state = value(&args, "--authority-state-dir")
            .ok_or("--desktop-sim-script requires --authority-state-dir")?;
        return run_desktop_sim_script(
            &mut host,
            &mut core,
            &footer,
            Path::new(session_socket),
            Path::new(authority_state),
        );
    }
    if let Some(authority_state) = value(&args, "--desktop-sim-supervise") {
        let session_socket = value(&args, "--session-socket").unwrap_or(DEFAULT_SESSION_SOCKET);
        return run_desktop_sim_supervisor(Path::new(session_socket), Path::new(authority_state));
    }
    if args.iter().any(|a| a == "--settings-evidence") {
        core.action(&ShellAction::Custom("Room.next".into()));
        core.action(&ShellAction::Custom("Room.next".into()));
        emit(&mut host, &mut core, &footer, out, "settings")?;
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        emit(&mut host, &mut core, &footer, out, "controls")?;
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        emit(&mut host, &mut core, &footer, out, "network")?;
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        emit(&mut host, &mut core, &footer, out, "system")?;
        core.reset_first_run();
        emit(&mut host, &mut core, &footer, out, "first-run")?;
        return Ok(());
    }
    emit(&mut host, &mut core, &footer, out, "boot-home")?;
    core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    emit(&mut host, &mut core, &footer, out, "focus-moved")?;
    let effect = core
        .action(&ShellAction::Activate)
        .ok_or("fixture must launch")?;
    emit(&mut host, &mut core, &footer, out, "launch-dimmed")?;
    let mut session = pf_ports::FakeSession::new(
        Ok(LaunchResult::Accepted {
            session_id: "fake-session".into(),
        }),
        [
            pf_ports::ScriptedSession::Event(SessionEvent::Observed(ObservedSessionState::Running)),
            pf_ports::ScriptedSession::Event(SessionEvent::Observed(
                ObservedSessionState::ObservationComplete,
            )),
            pf_ports::ScriptedSession::Event(SessionEvent::Terminal(TerminalReceipt::Returned {
                session_id: "fake-session".into(),
            })),
            pf_ports::ScriptedSession::Idle,
        ],
    );
    let Effect::Launch(request) = effect else {
        return Err("unexpected safe return".into());
    };
    core.launch_result(&session.launch(request).map_err(|e| format!("{e:?}"))?);
    core.drive_session(&mut session)
        .map_err(|e| format!("{e:?}"))?;
    emit(&mut host, &mut core, &footer, out, "returned")?;
    core.action(&ShellAction::Custom("Quick".into()));
    emit(&mut host, &mut core, &footer, out, "quick-power")?;
    emit_f10_evidence(&mut host, &snapshot, &theme, &footer, out)?;
    Ok(())
}

fn emit_f10_evidence(
    host: &mut OffscreenHost,
    snapshot: &CatalogSnapshot,
    theme: &pf_theme::Theme,
    footer: &str,
    out: &Path,
) -> Result<(), String> {
    let mut core = fixture_core(snapshot, theme, false);
    core.authority_snapshot(false);
    let start = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(10_000);
    core.load_history(&[HistoryEntry {
        session_id: "fictional-ridgeline-session".into(),
        item_id: "ridgeline".into(),
        receipt: None,
        started_at: Some(start),
        ended_at: Some(EndStamp {
            at: start + Duration::from_secs(3 * 60 * 60 + 20 * 60),
            precision: EndPrecision::Approximate,
        }),
    }]);
    core.action(&ShellAction::Custom("Room.next".into()));
    emit(host, &mut core, footer, out, "library")?;
    emit(host, &mut core, footer, out, "library-focused-search")?;
    core.action(&ShellAction::Custom("Search".into()));
    core.set_search_query("ridgeline");
    emit(host, &mut core, footer, out, "search")?;
    core.action(&ShellAction::Activate);
    emit(host, &mut core, footer, out, "details")?;

    let mut unavailable_snapshot = snapshot.clone();
    let unavailable_item = unavailable_snapshot
        .items
        .iter_mut()
        .find(|item| item.id == "steam-link")
        .ok_or("unavailable details fixture item missing")?;
    unavailable_item
        .variants
        .retain(|variant| !matches!(variant.availability, pf_catalog::Availability::Ready));
    let mut unavailable = fixture_core(&unavailable_snapshot, theme, false);
    unavailable.authority_snapshot(false);
    unavailable.action(&ShellAction::Custom("Room.next".into()));
    for _ in 0..8 {
        unavailable.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    }
    unavailable.action(&ShellAction::Activate);
    emit(host, &mut unavailable, footer, out, "details-unavailable")?;

    let mut chooser_snapshot = snapshot.clone();
    let item = chooser_snapshot
        .items
        .iter_mut()
        .find(|item| item.id == "moth-and-lantern")
        .ok_or("chooser fixture item missing")?;
    let mut second = item
        .variants
        .iter()
        .find(|variant| matches!(variant.availability, pf_catalog::Availability::Ready))
        .cloned()
        .ok_or("chooser fixture ready variant missing")?;
    second.id = "handheld".into();
    second.provider_id = "fixture-c".into();
    second.provenance.provider_id = "fixture-c".into();
    second.launch_target.app_id = "moth-and-lantern-handheld".into();
    item.variants.push(second);
    let mut chooser = fixture_core(&chooser_snapshot, theme, false);
    chooser.authority_snapshot(false);
    for _ in 0..3 {
        chooser.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    }
    chooser.action(&ShellAction::Activate);
    emit(host, &mut chooser, footer, out, "variant-chooser")?;
    Ok(())
}

fn emit_sim_frame(core: &mut ShellCore, prompt: &str, path: &Path) -> Result<(), String> {
    let width = env_dimension("PF_FB_WIDTH", 1280)?;
    let height = env_dimension("PF_FB_HEIGHT", 720)?;
    let stride = env_dimension("PF_FB_STRIDE", width * 4)?;
    if stride < width * 4 {
        return Err("PF_FB_STRIDE is smaller than one XRGB8888 row".into());
    }
    let logical_width = u16::try_from(width)
        .map(f32::from)
        .map_err(|_| "PF_FB_WIDTH exceeds the supported logical surface".to_owned())?;
    let logical_height = u16::try_from(height)
        .map(f32::from)
        .map_err(|_| "PF_FB_HEIGHT exceeds the supported logical surface".to_owned())?;
    let metrics = SurfaceMetrics {
        logical_width,
        logical_height,
        scale: 1.0,
        safe_insets: Insets::default(),
        orientation: if width >= height {
            Orientation::Landscape
        } else {
            Orientation::Portrait
        },
    };
    let mut host = OffscreenHost::new(metrics);
    apply_text_scale(&mut host, core)?;
    present(&mut host, core, prompt)?;
    let frame = host.frame().ok_or("sim frame missing")?;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    let padding = vec![0_u8; (stride - width * 4) as usize];
    for row in frame.rgba.chunks_exact(width as usize * 4) {
        for pixel in row.chunks_exact(4) {
            out.write_all(&[pixel[2], pixel[1], pixel[0], 0xff])
                .map_err(|e| e.to_string())?;
        }
        out.write_all(&padding).map_err(|e| e.to_string())?;
    }
    out.flush().map_err(|e| e.to_string())
}

fn env_dimension(name: &str, default: u32) -> Result<u32, String> {
    env::var(name).map_or(Ok(default), |value| {
        value.parse::<u32>().map_err(|e| format!("{name}: {e}"))
    })
}

trait InteractiveInput<H> {
    fn next_action(&mut self, host: &mut H, deadline: Deadline) -> Result<ActionPoll, String>;
    fn capture_next_button(&mut self);
    fn apply_effective_map(&mut self, map: &EffectiveMap);
}

struct EvdevInteractiveInput<'a> {
    source: &'a mut EvdevActionSource,
    repeat: KeyRepeatScheduler,
    pending: VecDeque<ShellAction>,
    started: Instant,
}

impl<'a> EvdevInteractiveInput<'a> {
    fn new(source: &'a mut EvdevActionSource) -> Self {
        Self {
            source,
            repeat: KeyRepeatScheduler::default(),
            pending: VecDeque::new(),
            started: Instant::now(),
        }
    }
}

impl InteractiveInput<FbdevHost> for EvdevInteractiveInput<'_> {
    fn next_action(
        &mut self,
        _host: &mut FbdevHost,
        deadline: Deadline,
    ) -> Result<ActionPoll, String> {
        let now = self.started.elapsed();
        match self.source.next_input_event(deadline) {
            Ok(Some(EvdevInputEvent::Pressed { code, action })) => {
                self.repeat.transition(
                    u32::from(code),
                    true,
                    action.clone(),
                    now,
                    EVDEV_REPEAT_DELAY,
                );
                self.pending.extend(action);
            }
            Ok(Some(EvdevInputEvent::Released { code })) => {
                self.repeat
                    .transition(u32::from(code), false, None, now, EVDEV_REPEAT_DELAY);
            }
            Ok(Some(EvdevInputEvent::ActiveSourceChanged)) => {
                self.repeat.clear();
                self.pending.clear();
            }
            Ok(None) => {}
            Err(error) => {
                self.repeat.clear();
                self.pending.clear();
                return Err(format!("input: {error:?}"));
            }
        }
        self.pending
            .extend(self.repeat.due(now, EVDEV_REPEAT_INTERVAL));
        self.pending
            .pop_front()
            .map_or(Ok(ActionPoll::DeadlineReached), |action| {
                Ok(ActionPoll::Event(ActionEvent::Action(action)))
            })
    }

    fn capture_next_button(&mut self) {
        self.repeat.clear();
        self.pending.clear();
        self.source.capture_next_button();
    }

    fn apply_effective_map(&mut self, map: &EffectiveMap) {
        self.repeat.clear();
        self.pending.clear();
        self.source.apply_effective_map(map);
    }
}

#[cfg(feature = "wayland")]
struct WaylandInteractiveInput {
    map: EffectiveMap,
    repeat: KeyRepeatScheduler,
    pending: VecDeque<ShellAction>,
    started: Instant,
}

#[cfg(feature = "wayland")]
trait WaylandInputHost {
    fn is_closed(&self) -> bool;
    fn repeat_info(&self) -> Option<RepeatInfo>;
    fn poll_key_event(&mut self) -> Option<KeyEvent>;
}

#[cfg(feature = "wayland")]
impl WaylandInputHost for WaylandHost {
    fn is_closed(&self) -> bool {
        self.is_closed()
    }

    fn repeat_info(&self) -> Option<RepeatInfo> {
        self.repeat_info()
    }

    fn poll_key_event(&mut self) -> Option<KeyEvent> {
        self.poll_key_event()
    }
}

#[cfg(feature = "wayland")]
impl WaylandInteractiveInput {
    fn new(map: EffectiveMap) -> Self {
        Self {
            map,
            repeat: KeyRepeatScheduler::default(),
            pending: VecDeque::new(),
            started: Instant::now(),
        }
    }
}

#[cfg(feature = "wayland")]
impl<H: WaylandInputHost> InteractiveInput<H> for WaylandInteractiveInput {
    fn next_action(&mut self, host: &mut H, _deadline: Deadline) -> Result<ActionPoll, String> {
        if host.is_closed() {
            self.repeat.clear();
            self.pending.clear();
            return Ok(ActionPoll::Closed);
        }
        let repeat_info = host.repeat_info().unwrap_or(RepeatInfo {
            rate: 25,
            delay_ms: 600,
        });
        let repeat_delay = if repeat_info.delay_ms >= 0 {
            Duration::from_millis(u64::try_from(repeat_info.delay_ms).expect("non-negative delay"))
        } else {
            Duration::ZERO
        };
        let repeat_interval = if repeat_info.rate > 0 {
            Duration::from_secs_f64(1.0 / f64::from(repeat_info.rate))
        } else {
            Duration::ZERO
        };
        let now = self.started.elapsed();
        while let Some(event) = host.poll_key_event() {
            let action = effective_keyboard_action(&self.map, event.key, event.keysym);
            let repeat_action = (repeat_info.rate > 0 && repeat_info.delay_ms >= 0)
                .then(|| action.clone())
                .flatten();
            self.repeat.transition(
                event.code,
                event.state == KeyState::Pressed,
                repeat_action,
                now,
                repeat_delay,
            );
            if event.state == KeyState::Pressed {
                self.pending.extend(action);
            }
        }
        self.pending.extend(self.repeat.due(now, repeat_interval));
        if let Some(action) = self.pending.pop_front() {
            Ok(ActionPoll::Event(ActionEvent::Action(action)))
        } else {
            thread::sleep(Duration::from_millis(5));
            Ok(ActionPoll::DeadlineReached)
        }
    }

    fn capture_next_button(&mut self) {}

    fn apply_effective_map(&mut self, map: &EffectiveMap) {
        self.map = map.clone();
    }
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_interactive<H: RenderedFrameHost, I: InteractiveInput<H>>(
    host: &mut H,
    actions: &mut I,
    core: &mut ShellCore,
    mut activate: String,
    preferences: &mut dyn PreferencePort,
    power: &mut dyn PowerPort,
    map: EffectiveMap,
    catalog: &dyn FavoriteCatalog,
    session_socket: &Path,
    network: &mut dyn NetworkPort,
    time: &mut dyn TimePort,
    transfer: &mut dyn TransferPort,
    state_dir: &Path,
    remap_store: JsonRemapStore,
) -> Result<(), String> {
    let deadline = Deadline(MonotonicTime::ZERO);
    let mut session = SessionClient::new(
        "pf-shell",
        SocketTransport::connect(session_socket.to_path_buf()),
    );
    match wait_for_session_authority(&mut session, Duration::from_secs(3)) {
        Ok(()) => drive_socket_session(core, &mut session)?,
        Err(SessionError::BackendUnavailable) => core.session_backend_unavailable_at_boot(),
        Err(error) => return Err(format!("session: {error:?}")),
    }
    apply_text_scale(host, core)?;
    present_interactive(host, core, &activate)?;
    let mut remap = GamepadRemap::with_store(map, remap_store);
    loop {
        let before = redraw_state(core);
        let poll = actions.next_action(host, deadline)?;
        drive_socket_session(core, &mut session)?;
        let ActionPoll::Event(ActionEvent::Action(action)) = poll else {
            if matches!(poll, ActionPoll::Closed) {
                return Ok(());
            }
            if before != redraw_state(core) {
                present_interactive(host, core, &activate)?;
            }
            continue;
        };
        match core.action(&action) {
            Some(Effect::SafeReturn) => {
                request_safe_return_if_active(core, &session);
                drive_socket_session(core, &mut session)?;
            }
            Some(Effect::Launch(request)) => match session.launch(request) {
                Ok(result) => {
                    core.session_backend_reachable();
                    core.launch_result(&result);
                    present_interactive(host, core, &activate)?;
                    drive_socket_session(core, &mut session)?;
                }
                Err(SessionError::BackendUnavailable) => core.session_backend_unavailable(),
                Err(error) => return Err(format!("session: {error:?}")),
            },
            Some(Effect::EnterRecovery) => return Ok(()),
            Some(Effect::ChangePreference(change)) => {
                preferences
                    .submit_change(change)
                    .map_err(|e| format!("preferences: {e:?}"))?;
                core.drive_preferences(preferences)
                    .map_err(|e| format!("preferences: {e:?}"))?;
                apply_text_scale(host, core)?;
            }
            Some(Effect::ResetFirstRun) => core.reset_first_run(),
            Some(Effect::CaptureRemap) => actions.capture_next_button(),
            Some(Effect::BeginRemap {
                context,
                action,
                control,
            }) => match remap.preview(&context, &action, pf_input_map::Binding::single(control)) {
                Ok(()) => {}
                Err(pf_input_map::MapError::Collision { second, .. }) => {
                    core.remap_refused(&second);
                }
                Err(error) => return Err(format!("remap preview: {error:?}")),
            },
            Some(Effect::ConfirmRemap) => {
                remap
                    .gamepad_action(&ShellAction::Activate)
                    .map_err(|e| format!("remap confirm: {e:?}"))?;
                core.remap_committed(control_bindings(remap.map()));
                actions.apply_effective_map(remap.map());
                activate = footer_prompt(remap.map());
            }
            Some(Effect::RollbackRemap) => {
                remap
                    .gamepad_action(&ShellAction::Back)
                    .map_err(|e| format!("remap rollback: {e:?}"))?;
            }
            Some(Effect::ResetRemaps) => {
                remap
                    .reset_defaults()
                    .map_err(|e| format!("remap reset: {e:?}"))?;
                core.remaps_reset(control_bindings(remap.map()));
                actions.apply_effective_map(remap.map());
                activate = footer_prompt(remap.map());
            }
            Some(Effect::CompleteFirstRun) => {
                preferences
                    .submit_change(PreferenceChange {
                        key: PreferenceKey("firstRunComplete".into()),
                        value: PreferenceValue::Bool(true),
                        authority: ChangeAuthority("user".into()),
                    })
                    .map_err(|e| format!("preferences: {e:?}"))?;
            }
            Some(Effect::ToggleFavorite { item_id, favorite }) => {
                match commit_favorite(catalog, &item_id, favorite) {
                    Ok(_) => core.favorite_committed(&item_id, favorite),
                    Err(status) => core.favorite_failed(status),
                }
            }
            Some(Effect::SetPinnedVariant {
                item_id,
                variant_id,
            }) => match commit_pinned_variant(catalog, &item_id, variant_id.as_deref()) {
                Ok(_) => core.pinned_variant_committed(&item_id, variant_id),
                Err(status) => core.pinned_variant_failed(status),
            },
            Some(Effect::CaptureScreenshot) => {
                let result = host
                    .raster_frame()
                    .ok_or_else(|| "composed frame is unavailable".to_owned())
                    .and_then(|frame| capture_screenshot(frame, state_dir, &FsScreenshotWriter));
                match result {
                    Ok(path) => {
                        let file_name = path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("screenshot.png");
                        core.screenshot_result(Ok(file_name));
                    }
                    Err(_) => core.screenshot_result(Err(())),
                }
            }
            Some(Effect::RequestPower(action)) => {
                let result = power.request(action);
                core.power_request_result(result);
            }
            Some(Effect::SetIdlePolicy(policy)) => {
                let result = power.set_idle_policy(policy);
                core.idle_policy_result(result);
            }
            Some(Effect::ConnectWifi { ssid, credential }) => {
                core.network_result(network.connect(&ssid, credential));
            }
            Some(Effect::SetTimezone(zone)) => core.timezone_result(time.set_timezone(zone)),
            Some(Effect::SetNtp(enabled)) => core.ntp_result(time.set_ntp_enabled(enabled)),
            Some(Effect::RefreshManualTime) => core.manual_time_refresh_result(time.read()),
            Some(Effect::SetManualTime(wall_clock)) => {
                core.manual_time_result(time.set_time(wall_clock));
            }
            Some(Effect::SetTransfer { service, enabled }) => {
                core.transfer_result(transfer.set_enabled(service, enabled));
            }
            None => {}
        }
        if before != redraw_state(core) {
            // Rasterizer damage tracking makes unchanged parts of the retained
            // scene a no-op at the fbdev boundary.
            present_interactive(host, core, &activate)?;
        }
    }
}

fn authority_rpc(
    socket: &Path,
    request: &pf_session_authority::RpcRequest,
) -> Result<pf_session_authority::RpcResponse, String> {
    let mut stream =
        UnixStream::connect(socket).map_err(|error| format!("authority rpc connect: {error}"))?;
    let body = serde_json::to_vec(request).map_err(|error| error.to_string())?;
    pf_wire::write_frame(&mut stream, &body).map_err(|error| error.to_string())?;
    let body = pf_wire::read_frame(&mut stream).map_err(|error| error.to_string())?;
    match serde_json::from_slice::<pf_session_authority::RpcResponse>(&body)
        .map_err(|error| error.to_string())?
    {
        pf_session_authority::RpcResponse::Error { message } => Err(message),
        response => Ok(response),
    }
}

fn observe_desktop_sim(
    socket: &Path,
    observation: pf_session_authority::RpcObservation,
) -> Result<(), String> {
    use pf_session_authority::{RpcRequest, RpcResponse};

    match authority_rpc(socket, &RpcRequest::Observe { observation }) {
        Ok(RpcResponse::Ok) => Ok(()),
        Err(message) if message == "InvalidObservation" => {
            println!("SUPERVISOR redundant observation={message}; continuing");
            Ok(())
        }
        Ok(response) => Err(format!("unexpected authority response: {response:?}")),
        Err(message) => Err(message),
    }
}

fn observe_desktop_sim_running(socket: &Path) -> Result<(), String> {
    use pf_session_authority::RpcObservation;

    observe_desktop_sim(socket, RpcObservation::SessionRunning)
}

fn observe_desktop_sim_return(socket: &Path, session_is_live: bool) -> Result<(), String> {
    use pf_session_authority::RpcObservation;

    let observations = [
        RpcObservation::SessionExitedCleanly,
        RpcObservation::UnitInactive,
        RpcObservation::TargetReleased,
        RpcObservation::SelectedOwnerActive,
        RpcObservation::PresentationAcknowledged,
    ];
    let first = usize::from(!session_is_live);
    for observation in observations.into_iter().skip(first) {
        observe_desktop_sim(socket, observation)?;
    }
    Ok(())
}

fn tick_desktop_sim(socket: &Path) -> Result<(), String> {
    use pf_session_authority::{RpcRequest, RpcResponse};

    match authority_rpc(socket, &RpcRequest::Tick)? {
        RpcResponse::Ok => Ok(()),
        response => Err(format!(
            "unexpected authority response to tick: {response:?}"
        )),
    }
}

fn authority_phase(authority_state: &Path) -> Result<Option<pf_session_authority::Phase>, String> {
    let path = authority_state.join("authority.json");
    let body = match fs::read(&path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", path.display())),
    };
    serde_json::from_slice::<pf_session_authority::PersistedState>(&body)
        .map(|state| Some(state.phase))
        .map_err(|error| format!("parse {}: {error}", path.display()))
}

fn marker_session_id(marker: &Path) -> Option<&str> {
    marker.file_name()?.to_str()?.strip_suffix(".running")
}

fn phase_is_stopping_session(phase: &pf_session_authority::Phase, session_id: &str) -> bool {
    matches!(
        phase,
        pf_session_authority::Phase::StoppingGracefully { session_id: active, .. }
            | pf_session_authority::Phase::ForceStopping { session_id: active }
            if active == session_id
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ObservableAuthorityPhase {
    Idle,
    Starting,
    Running,
    Returning,
}

fn observable_authority_phase(socket: &Path) -> Result<ObservableAuthorityPhase, String> {
    use pf_session_authority::{RpcRequest, RpcResponse};

    let RpcResponse::History { entries } = authority_rpc(socket, &RpcRequest::History)? else {
        return Err("unexpected authority response to history".into());
    };
    Ok(observable_phase_from_history(&entries))
}

fn observable_phase_from_history(entries: &[HistoryEntry]) -> ObservableAuthorityPhase {
    let Some(entry) = entries.iter().rev().find(|entry| entry.receipt.is_none()) else {
        return ObservableAuthorityPhase::Idle;
    };
    if entry.ended_at.is_some() {
        ObservableAuthorityPhase::Returning
    } else if entry.started_at.is_some() {
        ObservableAuthorityPhase::Running
    } else {
        ObservableAuthorityPhase::Starting
    }
}

fn desktop_sim_marker(authority_state: &Path) -> Result<Option<PathBuf>, String> {
    let sessions = authority_state.join("sessions");
    let entries = match fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("read {}: {error}", sessions.display())),
    };
    for entry in entries {
        let path = entry
            .map_err(|error| format!("read session marker: {error}"))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("running") {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn reconcile_desktop_sim_startup(
    socket: &Path,
    phase: ObservableAuthorityPhase,
    mut sample_phase: impl FnMut() -> Result<ObservableAuthorityPhase, String>,
    mut sample_marker: impl FnMut() -> Result<Option<PathBuf>, String>,
) -> Result<Option<PathBuf>, String> {
    let mut marker = sample_marker()?;
    if marker.is_none()
        && matches!(
            phase,
            ObservableAuthorityPhase::Starting
                | ObservableAuthorityPhase::Running
                | ObservableAuthorityPhase::Returning
        )
    {
        // A launch may publish its marker after the phase sample. Revalidate at the
        // last possible moment before treating the authority session as interrupted.
        marker = sample_marker()?;
    }
    let reconciled_phase = if marker.is_some() && phase == ObservableAuthorityPhase::Idle {
        sample_phase()?
    } else {
        phase
    };
    match (&marker, reconciled_phase) {
        (Some(path), ObservableAuthorityPhase::Starting) => {
            observe_desktop_sim_running(socket)?;
            println!("SUPERVISOR running marker={}", path.display());
        }
        (Some(path), ObservableAuthorityPhase::Running) => {
            println!("SUPERVISOR reconciled running marker={}", path.display());
        }
        (Some(path), ObservableAuthorityPhase::Returning) => {
            // Returning rejects SessionRunning, but the marker proves a new launch
            // raced the return. Adopt it without that invalid observation so the
            // watch loop tracks its eventual removal.
            println!("SUPERVISOR reconciled returning marker={}", path.display());
        }
        (Some(path), ObservableAuthorityPhase::Idle) => {
            fs::remove_file(path)
                .map_err(|error| format!("remove orphan marker {}: {error}", path.display()))?;
            println!("SUPERVISOR removed orphan marker={}", path.display());
            marker = None;
        }
        (
            None,
            ObservableAuthorityPhase::Starting
            | ObservableAuthorityPhase::Running
            | ObservableAuthorityPhase::Returning,
        ) => {
            observe_desktop_sim_return(socket, phase != ObservableAuthorityPhase::Returning)?;
            println!("SUPERVISOR reconciled return");
        }
        _ => {}
    }
    Ok(marker)
}

fn run_desktop_sim_supervisor(socket: &Path, authority_state: &Path) -> Result<(), String> {
    let phase = observable_authority_phase(socket)?;
    println!(
        "SUPERVISOR watching state_dir={} phase={phase:?}",
        authority_state.display(),
    );
    let marker = reconcile_desktop_sim_startup(
        socket,
        phase,
        || observable_authority_phase(socket),
        || desktop_sim_marker(authority_state),
    )?;
    let mut active_marker = marker;
    loop {
        tick_desktop_sim(socket)?;
        let phase = authority_phase(authority_state)?;
        if let (Some(path), Some(phase)) = (active_marker.as_ref(), phase.as_ref()) {
            if marker_session_id(path)
                .is_some_and(|session_id| phase_is_stopping_session(phase, session_id))
            {
                match fs::remove_file(path) {
                    Ok(()) => println!("SUPERVISOR stopped marker={}", path.display()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!("remove stopped marker {}: {error}", path.display()));
                    }
                }
            }
        }
        let marker = desktop_sim_marker(authority_state)?;
        match (&active_marker, marker) {
            (None, Some(path)) => {
                observe_desktop_sim_running(socket)?;
                println!("SUPERVISOR running marker={}", path.display());
                active_marker = Some(path);
            }
            (Some(path), None) => {
                let phase = observable_authority_phase(socket)?;
                observe_desktop_sim_return(socket, phase != ObservableAuthorityPhase::Returning)?;
                println!("SUPERVISOR returned marker={}", path.display());
                active_marker = None;
            }
            _ => {}
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn run_desktop_sim_script(
    host: &mut OffscreenHost,
    core: &mut ShellCore,
    footer: &str,
    socket: &Path,
    authority_state: &Path,
) -> Result<(), String> {
    let mut session = SessionClient::new("pf-shell-desktop-soak", SocketTransport::connect(socket));
    wait_for_session_authority(&mut session, Duration::from_secs(3))
        .map_err(|error| format!("authority ready: {error:?}"))?;
    drive_socket_session(core, &mut session)?;
    present(host, core, footer)?;
    let initial_revision = redraw_state(core);

    let _ = core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    let request = match core.action(&ShellAction::Activate) {
        Some(Effect::Launch(request)) => request,
        other => return Err(format!("scripted launch action produced {other:?}")),
    };
    let session_id = match session.launch(request) {
        Ok(LaunchResult::Accepted { session_id }) => session_id,
        Ok(other) => return Err(format!("authority rejected scripted launch: {other:?}")),
        Err(error) => return Err(format!("authority launch: {error:?}")),
    };
    core.launch_result(&LaunchResult::Accepted {
        session_id: session_id.clone(),
    });
    present(host, core, footer)?;
    println!("SOAK launched session_id={session_id}");

    let marker = authority_state
        .join("sessions")
        .join(format!("{session_id}.running"));
    let marker_deadline = Instant::now() + Duration::from_secs(3);
    while !marker.is_file() {
        if Instant::now() >= marker_deadline {
            return Err(format!(
                "desktop-sim marker did not appear: {}",
                marker.display()
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    println!("SOAK marker={}", marker.display());

    observe_desktop_sim_running(socket)?;
    drive_socket_session(core, &mut session)?;

    if core.action(&ShellAction::Custom("SafeReturn".into())) != Some(Effect::SafeReturn) {
        return Err("scripted safe return action was unavailable".into());
    }
    request_safe_return_if_active(core, &session);
    let return_deadline = Instant::now() + Duration::from_secs(3);
    while !matches!(
        authority_phase(authority_state)?.as_ref(),
        Some(pf_session_authority::Phase::Idle)
    ) {
        if Instant::now() >= return_deadline {
            return Err("desktop-sim safe return did not reach Idle".into());
        }
        drive_socket_session(core, &mut session)?;
        thread::sleep(Duration::from_millis(10));
    }
    drive_socket_session(core, &mut session)?;
    present(host, core, footer)?;
    if initial_revision == redraw_state(core) {
        return Err("redraw state did not advance across launch/return".into());
    }
    if marker.exists() {
        return Err(format!("desktop-sim marker remains: {}", marker.display()));
    }
    if !authority_state.join("shell-selected").is_file() {
        return Err("authority did not reactivate the shell owner".into());
    }
    let returned = session.history().iter().any(|event| {
        matches!(
            event,
            SessionEvent::Terminal(TerminalReceipt::Returned { session_id: returned })
                if returned == &session_id
        )
    });
    if !returned {
        return Err("authority returned receipt was not consumed".into());
    }
    println!("SOAK returned session_id={session_id} redraw_advanced=true state_clean=true");
    Ok(())
}

trait ScreenshotWriter {
    fn write_png(&self, path: &Path, frame: &RasterFrame) -> Result<(), String>;
}

struct FsScreenshotWriter;

static SCREENSHOT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl ScreenshotWriter for FsScreenshotWriter {
    fn write_png(&self, path: &Path, frame: &RasterFrame) -> Result<(), String> {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| format!("invalid screenshot path: {}", path.display()))?;
        let sequence = SCREENSHOT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = path.with_file_name(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            sequence
        ));
        let result = (|| {
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)
                .map_err(|error| error.to_string())?;
            write_png(file, frame)?;
            fs::rename(&temp_path, path).map_err(|error| error.to_string())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }
}

fn write_png(sink: impl Write, frame: &RasterFrame) -> Result<(), String> {
    let mut buf = BufWriter::new(sink);
    let mut encoder = png::Encoder::new(&mut buf, frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
    writer
        .write_image_data(&frame.rgba)
        .map_err(|error| error.to_string())?;
    writer.finish().map_err(|error| error.to_string())?;
    buf.flush().map_err(|error| error.to_string())
}

fn capture_screenshot(
    frame: &RasterFrame,
    state_dir: &Path,
    writer: &dyn ScreenshotWriter,
) -> Result<PathBuf, String> {
    fs::create_dir_all(state_dir).map_err(|error| error.to_string())?;
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    let path = state_dir.join(format!("screenshot-{timestamp}.png"));
    writer.write_png(&path, frame)?;
    Ok(path)
}

struct UnavailablePowerPort;

impl PowerPort for UnavailablePowerPort {
    fn capabilities(&self) -> Result<Vec<PowerCapability>, PowerError> {
        Err(PowerError::BackendUnavailable)
    }

    fn request(&mut self, _action: PowerAction) -> Result<PowerRequestResult, PowerError> {
        Err(PowerError::BackendUnavailable)
    }

    fn idle_policy(&self) -> Result<IdlePolicy, PowerError> {
        Err(PowerError::BackendUnavailable)
    }

    fn set_idle_policy(
        &mut self,
        _policy: IdlePolicy,
    ) -> Result<pf_ports::AppliedIdlePolicy, PowerError> {
        Err(PowerError::BackendUnavailable)
    }
}

struct UnavailableNetworkPort;

impl NetworkPort for UnavailableNetworkPort {
    fn state(&self) -> Result<NetworkState, NetworkError> {
        Err(NetworkError::BackendUnavailable)
    }

    fn scan(&mut self) -> Result<Vec<WifiNetwork>, NetworkError> {
        Err(NetworkError::BackendUnavailable)
    }

    fn connect(
        &mut self,
        _ssid: &str,
        _credential: WifiCredential,
    ) -> Result<pf_ports::ConnectResult, NetworkError> {
        Err(NetworkError::BackendUnavailable)
    }

    fn forget(&mut self, _ssid: &str) -> Result<bool, NetworkError> {
        Err(NetworkError::BackendUnavailable)
    }

    fn set_enabled(&mut self, _enabled: bool) -> Result<AppliedNetworkEnabled, NetworkError> {
        Err(NetworkError::BackendUnavailable)
    }
}

struct UnavailableTimePort;

impl TimePort for UnavailableTimePort {
    fn capabilities(&self) -> Result<TimeCapabilities, TimeError> {
        Err(TimeError::BackendUnavailable)
    }

    fn read(&self) -> Result<TimeState, TimeError> {
        Err(TimeError::BackendUnavailable)
    }

    fn set_timezone(&mut self, _timezone: String) -> Result<AppliedValue<String>, TimeError> {
        Err(TimeError::BackendUnavailable)
    }

    fn set_ntp_enabled(&mut self, _enabled: bool) -> Result<AppliedValue<bool>, TimeError> {
        Err(TimeError::BackendUnavailable)
    }

    fn set_time(
        &mut self,
        _wall_clock: std::time::SystemTime,
    ) -> Result<AppliedValue<std::time::SystemTime>, TimeError> {
        Err(TimeError::BackendUnavailable)
    }
}

struct UnavailableTransferPort;

impl TransferPort for UnavailableTransferPort {
    fn services(&self) -> Result<Vec<TransferServiceState>, TransferError> {
        Err(TransferError::BackendUnavailable)
    }

    fn set_enabled(
        &mut self,
        _service: TransferService,
        _enabled: bool,
    ) -> Result<AppliedTransferState, TransferError> {
        Err(TransferError::BackendUnavailable)
    }
}

fn redraw_state(
    core: &ShellCore,
) -> (
    pf_shell_core::Presentation,
    usize,
    bool,
    Option<String>,
    u64,
) {
    (
        core.presentation().clone(),
        core.focus(),
        core.authority_unavailable(),
        core.session_status().map(str::to_owned),
        core.revision(),
    )
}

fn wait_for_session_authority(
    session: &mut dyn SessionPort,
    timeout: Duration,
) -> Result<(), SessionError> {
    let until = Instant::now() + timeout;
    loop {
        match session.next_event(Deadline(MonotonicTime::ZERO)) {
            Ok(_) => return Ok(()),
            Err(SessionError::BackendUnavailable) if Instant::now() < until => {
                thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return Err(error),
        }
    }
}

fn drive_socket_session(
    core: &mut ShellCore,
    session: &mut SessionClient<SocketTransport>,
) -> Result<(), String> {
    refresh_history(core, session);
    loop {
        match session.next_event(Deadline(MonotonicTime::ZERO)) {
            Ok(SessionPoll::Event(event)) => {
                core.session_backend_reachable();
                core.session_event(&event);
                if session.acknowledge_last().is_err() {
                    session_backend_unavailable(core);
                    break;
                }
                if matches!(
                    core.presentation(),
                    pf_shell_core::Presentation::RecoveryRequired
                ) {
                    break;
                }
            }
            Ok(SessionPoll::Idle | SessionPoll::DeadlineReached) => {
                core.session_backend_reachable();
                refresh_history(core, session);
                break;
            }
            Err(SessionError::BackendUnavailable) => {
                session_backend_unavailable(core);
                break;
            }
            Err(error) => return Err(format!("session: {error:?}")),
        }
    }
    Ok(())
}

fn request_safe_return_if_active(core: &mut ShellCore, session: &SessionClient<SocketTransport>) {
    if !core.session_active() {
        return;
    }
    match session.safe_return() {
        Ok(()) => {
            core.safe_return_succeeded();
            core.session_backend_reachable();
        }
        Err(_) => core.safe_return_failed(),
    }
}

fn session_backend_unavailable(core: &mut ShellCore) {
    if core.session_active() {
        core.active_session_backend_unavailable();
    } else {
        core.session_backend_unavailable();
    }
}

fn refresh_history(core: &mut ShellCore, session: &mut SessionClient<SocketTransport>) {
    if let Ok(entries) = session.transport_mut().history_entries() {
        core.load_history(&entries);
    }
}

fn fixture_preferences() -> FakePreferencePort {
    let values = [
        ("textScale", PreferenceValue::Text("100%".into())),
        ("highContrast", PreferenceValue::Bool(false)),
        ("reduceMotion", PreferenceValue::Bool(false)),
        ("reduceFlashing", PreferenceValue::Bool(false)),
        ("appearance", PreferenceValue::Text("Dusk".into())),
    ]
    .into_iter()
    .map(|(key, value)| EffectivePreference {
        key: PreferenceKey(key.into()),
        effective: value.clone(),
        stored: value,
        applied: true,
    });
    FakePreferencePort::new(values, ChangeAuthority("user".into()))
}

struct DurablePreferences {
    inner: PrefsPreferencePort,
    state_file: PathBuf,
    pending: VecDeque<EffectivePreference>,
}

impl DurablePreferences {
    fn open(state_dir: &Path) -> Result<Self, String> {
        let store = PrefsStore::at(state_dir);
        let state_file = store.path().to_owned();
        let inner =
            PrefsPreferencePort::for_user(store).map_err(|e| format!("preferences: {e:?}"))?;
        Ok(Self {
            inner,
            // Launcher-owned forward-compatible keys live in the same atomic preference
            // document. `pf-prefs` preserves unknown keys when it updates schema-owned values.
            state_file,
            pending: VecDeque::new(),
        })
    }

    fn launcher_state(
        &self,
    ) -> Result<serde_json::Map<String, serde_json::Value>, PreferenceError> {
        match fs::read_to_string(&self.state_file) {
            Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|value| value.as_object().cloned())
                .ok_or(PreferenceError::BackendUnavailable),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(serde_json::Map::default())
            }
            Err(_) => Err(PreferenceError::BackendUnavailable),
        }
    }

    fn write_launcher_state(
        &self,
        state: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), PreferenceError> {
        let parent = self
            .state_file
            .parent()
            .ok_or(PreferenceError::BackendUnavailable)?;
        fs::create_dir_all(parent).map_err(|_| PreferenceError::BackendUnavailable)?;
        let temporary = self
            .state_file
            .with_extension(format!("json.tmp.{}", std::process::id()));
        let bytes =
            serde_json::to_vec_pretty(state).map_err(|_| PreferenceError::BackendUnavailable)?;
        fs::write(&temporary, bytes).map_err(|_| PreferenceError::BackendUnavailable)?;
        fs::rename(temporary, &self.state_file).map_err(|_| PreferenceError::BackendUnavailable)
    }

    fn first_run_complete(&self) -> Result<bool, String> {
        self.launcher_state()
            .map(|state| {
                state
                    .get("firstRunComplete")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .map_err(|e| format!("preferences: {e:?}"))
    }

    fn safe_return_binding(&self) -> Result<Option<String>, String> {
        self.launcher_state()
            .map(|state| {
                state
                    .get("safeReturnBinding")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .map_err(|e| format!("preferences: {e:?}"))
    }
}

impl PreferencePort for DurablePreferences {
    fn read(&self, key: &PreferenceKey) -> Result<Option<EffectivePreference>, PreferenceError> {
        if key.0 == "appearance" {
            let value = self
                .launcher_state()?
                .get("appearance")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Dusk")
                .to_owned();
            return Ok(Some(EffectivePreference {
                key: key.clone(),
                effective: PreferenceValue::Text(value.clone()),
                stored: PreferenceValue::Text(value),
                applied: true,
            }));
        }
        if matches!(key.0.as_str(), "firstRunComplete" | "safeReturnBinding") {
            return Ok(None);
        }
        self.inner.read(key).map(|value| {
            value.map(|mut observed| {
                observed.effective = observed.stored.clone();
                observed.applied = true;
                observed
            })
        })
    }

    fn next_change(&mut self, deadline: Deadline) -> Result<PreferencePoll, PreferenceError> {
        if let Some(change) = self.pending.pop_front() {
            return Ok(PreferencePoll::Changed(change));
        }
        self.inner.next_change(deadline).map(|poll| match poll {
            PreferencePoll::Changed(mut change) => {
                change.effective = change.stored.clone();
                change.applied = true;
                PreferencePoll::Changed(change)
            }
            other => other,
        })
    }

    fn submit_change(
        &mut self,
        change: PreferenceChange,
    ) -> Result<PreferenceChangeResult, PreferenceError> {
        if matches!(
            change.key.0.as_str(),
            "firstRunComplete" | "safeReturnBinding" | "appearance"
        ) {
            if change.authority != ChangeAuthority("user".into()) {
                return Ok(PreferenceChangeResult::Unauthorized);
            }
            let mut state = self.launcher_state()?;
            let effective = change.value.clone();
            let value = match change.value {
                PreferenceValue::Bool(value) => serde_json::Value::Bool(value),
                PreferenceValue::Text(value) => serde_json::Value::String(value),
                PreferenceValue::Integer(value) => serde_json::Value::Number(value.into()),
            };
            let key = change.key;
            state.insert(key.0.clone(), value);
            self.write_launcher_state(&state)?;
            if key.0 == "appearance" {
                self.pending.push_back(EffectivePreference {
                    key,
                    effective: effective.clone(),
                    stored: effective,
                    applied: true,
                });
            }
            return Ok(PreferenceChangeResult::Accepted);
        }
        let key = change.key.clone();
        let result = self.inner.submit_change(change)?;
        if matches!(
            result,
            PreferenceChangeResult::StoredNotApplied | PreferenceChangeResult::Accepted
        ) {
            if let Some(mut observed) = self.inner.read(&key)? {
                observed.effective = observed.stored.clone();
                observed.applied = true;
                self.pending.push_back(observed);
            }
            return Ok(PreferenceChangeResult::Accepted);
        }
        Ok(result)
    }
}

fn emit(
    host: &mut OffscreenHost,
    core: &mut ShellCore,
    prompt: &str,
    out: &Path,
    name: &str,
) -> Result<(), String> {
    let scene = core
        .scene(host.metrics(), prompt)
        .ok_or("shell has no frame")?;
    present_scene(host, core, prompt, &scene)?;
    let frame = host.frame().ok_or("frame missing")?.clone();
    if env::var_os("PF_RASTER_INK_GUARD").is_some() {
        assert_raster_text_legible(&scene, host.metrics(), core.theme_base(), core.text_scale())?;
    }
    let path = out.join(format!("{name}.png"));
    let file = fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), frame.width, frame.height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .map_err(|e| e.to_string())?
        .write_image_data(&frame.rgba)
        .map_err(|e| e.to_string())?;
    println!("{}  {}", hex(&Sha256::digest(&frame.rgba)), path.display());
    Ok(())
}

fn fails_raster_text_floor(
    node: &Node,
    ink_pixels: usize,
    rendered_ratio: f64,
    floor: f64,
) -> bool {
    ink_pixels == 0 || (!node.state.disabled && rendered_ratio < floor)
}

fn core_ink_low_tail(samples: &[Option<(f64, f64)>]) -> Option<f64> {
    let mut ratios = Vec::new();
    for &(ratio, coverage) in samples.iter().flatten() {
        if coverage >= 0.8 {
            ratios.push(ratio);
        }
    }
    ratios.sort_by(f64::total_cmp);
    ratios.get(ratios.len().saturating_sub(1) / 10).copied()
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines
)]
fn assert_raster_text_legible(
    scene: &pf_scene::Scene,
    metrics: SurfaceMetrics,
    base: pf_theme::Base,
    text_scale: u16,
) -> Result<(), String> {
    fn suppress_text(node: &mut Node, target: &pf_scene::NodeId) {
        if &node.id == target {
            node.role = Role::Group;
            return;
        }
        for child in &mut node.children {
            suppress_text(child, target);
        }
    }
    fn luminance(color: [u8; 3]) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(color[0]) + 0.7152 * channel(color[1]) + 0.0722 * channel(color[2])
    }
    fn contrast(a: [u8; 3], b: [u8; 3]) -> f64 {
        let (light, dark) = if luminance(a) >= luminance(b) {
            (luminance(a), luminance(b))
        } else {
            (luminance(b), luminance(a))
        };
        (light + 0.05) / (dark + 0.05)
    }
    fn rgb(value: &str) -> [u8; 3] {
        [1, 3, 5].map(|offset| u8::from_str_radix(&value[offset..offset + 2], 16).unwrap())
    }
    fn coverage(ink: [u8; 3], background: [u8; 3], foreground: [u8; 3]) -> f64 {
        let mut projection = 0.0;
        let mut magnitude = 0.0;
        for channel in 0..3 {
            let direction = f64::from(foreground[channel]) - f64::from(background[channel]);
            projection += (f64::from(ink[channel]) - f64::from(background[channel])) * direction;
            magnitude += direction * direction;
        }
        if magnitude == 0.0 {
            0.0
        } else {
            (projection / magnitude).clamp(0.0, 1.0)
        }
    }
    fn visit(
        node: &Node,
        frame: &RasterFrame,
        scene: &pf_scene::Scene,
        render: &impl Fn(&pf_scene::Scene) -> Result<RasterFrame, String>,
        base: pf_theme::Base,
        floor: f64,
        failures: &mut Vec<String>,
    ) -> Result<(), String> {
        if matches!(node.role, Role::Text | Role::Heading)
            && !node.accessible_label.trim().is_empty()
        {
            let mut root = scene.root().clone();
            suppress_text(&mut root, &node.id);
            let suppressed_scene = pf_scene::Scene::new(root, scene.default_focus().clone())
                .map_err(|error| error.to_string())?;
            let suppressed = render(&suppressed_scene)?;
            if (frame.width, frame.height) != (suppressed.width, suppressed.height) {
                return Err("raster guard frame dimensions changed between paired renders".into());
            }
            let left = node.bounds.x.max(0.0) as u32;
            let top = node.bounds.y.max(0.0) as u32;
            let right = (node.bounds.x + node.bounds.width).ceil().max(0.0) as u32;
            let bottom = (node.bounds.y + node.bounds.height).ceil().max(0.0) as u32;
            let mut ink_pixels = 0_usize;
            let sample_width = right.min(frame.width).saturating_sub(left) as usize;
            let sample_height = bottom.min(frame.height).saturating_sub(top) as usize;
            let mut samples = vec![None; sample_width * sample_height];
            let text_token = if node.state.disabled {
                "--state-disabled-text"
            } else if node.state.unavailable {
                "--state-unavailable-text"
            } else if node.state.focused {
                "--state-focused-text"
            } else {
                "--state-rest-text"
            };
            let foreground = rgb(pf_theme::flagship().resolve(base, text_token).unwrap());
            for y in top..bottom.min(frame.height) {
                for x in left..right.min(frame.width) {
                    let offset = ((y * frame.width + x) * 4) as usize;
                    let ink = &frame.rgba[offset..offset + 3];
                    let background = &suppressed.rgba[offset..offset + 3];
                    if ink != background {
                        ink_pixels += 1;
                        let sample = (y - top) as usize * sample_width + (x - left) as usize;
                        let ink = [ink[0], ink[1], ink[2]];
                        let background = [background[0], background[1], background[2]];
                        samples[sample] = Some((
                            contrast(ink, background),
                            coverage(ink, background, foreground),
                        ));
                    }
                }
            }
            // Compare the route contribution with the node's own isolated ink.  A
            // rounded later fill can leave a few corner pixels visible; treating that
            // sliver as legible merely because it is non-zero misses the real failure.
            let isolated_scene = pf_scene::Scene::new(node.clone(), node.id.clone())
                .map_err(|error| error.to_string())?;
            let isolated = render(&isolated_scene)?;
            let mut isolated_root = node.clone();
            suppress_text(&mut isolated_root, &node.id);
            let isolated_suppressed_scene = pf_scene::Scene::new(isolated_root, node.id.clone())
                .map_err(|error| error.to_string())?;
            let isolated_suppressed = render(&isolated_suppressed_scene)?;
            let isolated_ink_pixels = isolated
                .rgba
                .chunks_exact(4)
                .zip(isolated_suppressed.rgba.chunks_exact(4))
                .filter(|(painted, suppressed)| painted != suppressed)
                .count();
            let occluded =
                isolated_ink_pixels > 0 && ink_pixels.saturating_mul(5) < isolated_ink_pixels;
            let rendered_ratio = core_ink_low_tail(&samples).unwrap_or_default();
            // High coverage structurally removes the anti-aliased fringe. The low
            // decile then requires the glyph body, rather than a high-contrast
            // minority, to clear the rendered contrast floor. Every sample comes
            // from the rendered and text-suppressed rasters, never tokens.
            // Disabled text has no available action and is exempt from the text
            // contrast floor, but it must still produce visible raster ink.
            if occluded {
                failures.push(format!(
                    "{} ({:?}, {:?}): occluded-by-later-paint, ink_pixels={ink_pixels}, isolated_ink_pixels={isolated_ink_pixels}",
                    node.id.as_str(),
                    node.accessible_label,
                    node.bounds,
                ));
            } else if fails_raster_text_floor(node, ink_pixels, rendered_ratio, floor) {
                failures.push(format!(
                    "{} ({:?}, {:?}): ink_pixels={ink_pixels}, raster_contrast={rendered_ratio:.2}, required={floor:.1}",
                    node.id.as_str(),
                    node.accessible_label,
                    node.bounds,
                ));
            }
        }
        for child in &node.children {
            visit(child, frame, scene, render, base, floor, failures)?;
        }
        Ok(())
    }

    let render = |scene: &pf_scene::Scene| -> Result<RasterFrame, String> {
        let mut host = OffscreenHost::new(metrics);
        host.set_theme_base(base);
        host.set_text_scale(f32::from(text_scale) / 100.0)
            .map_err(|error| format!("render: {error:?}"))?;
        host.present(scene).map_err(|error| error.to_string())?;
        host.frame()
            .cloned()
            .ok_or("raster guard frame missing".into())
    };
    let frame = render(scene)?;
    let mut failures = Vec::new();
    let floor = if base == pf_theme::Base::HighContrast {
        7.0
    } else {
        4.5
    };
    visit(
        scene.root(),
        &frame,
        scene,
        &render,
        base,
        floor,
        &mut failures,
    )?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("raster ink guard failed:\n{}", failures.join("\n")))
    }
}

fn failed_source_ids(notes: &[RenderNote]) -> Vec<&str> {
    notes
        .iter()
        .map(|note| match note {
            RenderNote::ImageDecodeFailed { source_id }
            | RenderNote::ImageTooLarge { source_id, .. } => source_id.as_str(),
        })
        .collect()
}

trait RenderedFrameHost: FrameHost {
    fn set_text_scale(&mut self, factor: f32) -> Result<(), String>;
    fn render_notes(&self) -> Option<&[RenderNote]>;
    fn raster_frame(&self) -> Option<&RasterFrame>;
}

impl RenderedFrameHost for OffscreenHost {
    fn set_text_scale(&mut self, factor: f32) -> Result<(), String> {
        OffscreenHost::set_text_scale(self, factor).map_err(|error| format!("render: {error:?}"))
    }

    fn render_notes(&self) -> Option<&[RenderNote]> {
        self.frame().map(|frame| frame.notes.as_slice())
    }

    fn raster_frame(&self) -> Option<&RasterFrame> {
        self.frame()
    }
}

impl RenderedFrameHost for FbdevHost {
    fn set_text_scale(&mut self, factor: f32) -> Result<(), String> {
        FbdevHost::set_text_scale(self, factor).map_err(|error| format!("render: {error:?}"))
    }

    fn render_notes(&self) -> Option<&[RenderNote]> {
        self.frame().map(|frame| frame.notes.as_slice())
    }

    fn raster_frame(&self) -> Option<&RasterFrame> {
        self.frame()
    }
}

#[cfg(feature = "wayland")]
impl RenderedFrameHost for WaylandHost {
    fn set_text_scale(&mut self, factor: f32) -> Result<(), String> {
        WaylandHost::set_text_scale(self, factor).map_err(|error| format!("render: {error:?}"))
    }

    fn render_notes(&self) -> Option<&[RenderNote]> {
        None
    }

    fn raster_frame(&self) -> Option<&RasterFrame> {
        None
    }
}

fn apply_text_scale(host: &mut impl RenderedFrameHost, core: &ShellCore) -> Result<(), String> {
    host.set_text_scale(f32::from(core.text_scale()) / 100.0)
}

fn present(
    host: &mut impl RenderedFrameHost,
    core: &mut ShellCore,
    prompt: &str,
) -> Result<(), String> {
    let scene = core
        .scene(host.metrics(), prompt)
        .ok_or("shell has no frame")?;
    present_scene(host, core, prompt, &scene)
}

fn present_interactive(
    host: &mut impl RenderedFrameHost,
    core: &mut ShellCore,
    prompt: &str,
) -> Result<bool, String> {
    let Some(scene) = core.scene(host.metrics(), prompt) else {
        // While a session is active, the foreground app owns presentation. Keep
        // polling input, authority events, and host lifecycle without committing
        // a replacement shell frame.
        return Ok(false);
    };
    present_scene(host, core, prompt, &scene)?;
    Ok(true)
}

fn present_scene(
    host: &mut impl RenderedFrameHost,
    core: &mut ShellCore,
    prompt: &str,
    scene: &pf_scene::Scene,
) -> Result<(), String> {
    host.set_theme_base(core.theme_base());
    ensure_action_labels(scene)?;
    host.present(scene).map_err(|e| e.to_string())?;
    let rejected = host
        .render_notes()
        .is_some_and(|notes| core.reject_art_sources(failed_source_ids(notes)));
    if rejected {
        let fallback = core
            .scene(host.metrics(), prompt)
            .ok_or("shell has no fallback frame")?;
        ensure_action_labels(&fallback)?;
        host.present(&fallback).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn ensure_action_labels(scene: &pf_scene::Scene) -> Result<(), String> {
    fn contains(outer: pf_scene::Bounds, inner: pf_scene::Bounds) -> bool {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.width <= outer.x + outer.width
            && inner.y + inner.height <= outer.y + outer.height
    }

    fn has_name_ink(node: &Node, action_bounds: pf_scene::Bounds) -> bool {
        node.children.iter().any(|child| {
            (matches!(child.role, Role::Text | Role::Heading)
                && !child.accessible_label.trim().is_empty()
                && contains(action_bounds, child.bounds))
                || has_name_ink(child, action_bounds)
        })
    }

    fn visit(node: &Node) -> Result<(), String> {
        if node.action.is_some() && node.accessible_label.trim().is_empty() {
            return Err(format!(
                "action has no accessible label on {}",
                node.id.as_str()
            ));
        }
        if node.action.is_some()
            && !node.accessible_label.trim().is_empty()
            && !has_name_ink(node, node.bounds)
        {
            return Err(format!(
                "action has no explicit in-bounds name ink on {}",
                node.id.as_str()
            ));
        }
        node.children.iter().try_for_each(visit)
    }

    visit(scene.root())
}

fn value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str())
}

fn validate_args(args: &[String]) -> Result<(), String> {
    #[cfg(not(feature = "wayland"))]
    if args.iter().any(|arg| arg == "--wayland") {
        return Err(
            "--wayland requires a build with the 'wayland' feature (cargo build --features wayland)"
                .into(),
        );
    }
    if args.iter().any(|arg| arg == "--fbdev")
        && args.iter().any(|arg| arg == "--settings-evidence")
    {
        return Err("usage error: --fbdev conflicts with --settings-evidence".into());
    }
    if value(args, "--catalog-root").is_some() && value(args, "--catalog-snapshot").is_some() {
        return Err(
            "usage error: --catalog-root and --catalog-snapshot cannot be used together".into(),
        );
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        out
    })
}

#[cfg(test)]
mod durable_tests {
    use super::*;

    fn contrast_probe(state: fn(&mut Node)) -> Node {
        let mut label = Node::new(
            pf_scene::NodeId::new("contrast-probe-label").unwrap(),
            Role::Text,
            "Low contrast",
            pf_scene::Bounds::new(20.0, 20.0, 180.0, 48.0),
            "--state-rest-text",
        );
        state(&mut label);
        label
    }

    #[test]
    fn raster_ink_guard_rejects_a_sub_floor_text_pair() {
        let label = contrast_probe(|_| {});
        assert!(fails_raster_text_floor(&label, 1, 4.0, 4.5));
        assert!(fails_raster_text_floor(&label, 1, 6.9, 7.0));
    }

    fn synthetic_ink(
        width: usize,
        height: usize,
        ratio: impl Fn(usize) -> f64,
    ) -> Vec<Option<(f64, f64)>> {
        let mut samples = vec![None; width * height];
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                samples[y * width + x] = Some((ratio(x), 1.0));
            }
        }
        samples
    }

    #[test]
    fn raster_ink_guard_rejects_mixed_background_core_ink() {
        let samples = synthetic_ink(12, 7, |x| if x < 6 { 5.0 } else { 4.0 });
        let ratio = core_ink_low_tail(&samples).unwrap();
        assert!(ratio < 4.5, "low-tail core contrast was {ratio}");
    }

    #[test]
    fn raster_ink_guard_accepts_uniform_passing_core_with_aa_fringe() {
        let mut samples = synthetic_ink(12, 7, |_| 5.0);
        for x in 1..11 {
            samples[12 + x] = Some((1.1, 0.3));
            samples[5 * 12 + x] = Some((1.1, 0.3));
        }
        for y in 1..6 {
            samples[y * 12 + 1] = Some((1.1, 0.3));
            samples[y * 12 + 10] = Some((1.1, 0.3));
        }
        let ratio = core_ink_low_tail(&samples).unwrap();
        assert!(ratio >= 4.5, "AA fringe lowered core contrast to {ratio}");
    }

    #[test]
    fn raster_ink_guard_rejects_uniform_sub_floor_core_ink() {
        let samples = synthetic_ink(12, 7, |_| 4.0);
        let ratio = core_ink_low_tail(&samples).unwrap();
        assert!(ratio < 4.5, "sub-floor core contrast was {ratio}");
    }

    #[test]
    fn raster_ink_guard_exempts_disabled_text_from_the_floor() {
        let label = contrast_probe(|node| node.state.disabled = true);
        assert!(!fails_raster_text_floor(&label, 1, 4.0, 4.5));
        assert!(fails_raster_text_floor(&label, 0, 4.0, 4.5));
    }

    #[test]
    fn raster_ink_guard_does_not_exempt_unavailable_text() {
        let label = contrast_probe(|node| node.state.unavailable = true);
        assert!(fails_raster_text_floor(&label, 1, 4.0, 4.5));
    }

    #[test]
    fn raster_ink_guard_attributes_overlapping_ink_to_each_text_node() {
        let bounds = pf_scene::Bounds::new(20.0, 20.0, 180.0, 48.0);
        let root_id = pf_scene::NodeId::new("visible-overlap-label").unwrap();
        let root = Node::new(
            root_id.clone(),
            Role::Text,
            "Visible ink",
            bounds,
            "--state-rest-text",
        )
        .with_children(vec![Node::new(
            pf_scene::NodeId::new("inkless-overlap-label").unwrap(),
            Role::Text,
            "\u{200d}",
            bounds,
            "--state-rest-text",
        )]);
        let scene = pf_scene::Scene::new(root, root_id).unwrap();
        let metrics = SurfaceMetrics {
            logical_width: 240.0,
            logical_height: 96.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };

        let failure =
            assert_raster_text_legible(&scene, metrics, pf_theme::Base::Dusk, 100).unwrap_err();
        assert!(
            failure.contains("inkless-overlap-label") && failure.contains("ink_pixels=0"),
            "unexpected guard verdict: {failure}"
        );
    }

    #[test]
    fn raster_ink_guard_rejects_text_fully_occluded_by_later_fill() {
        let bounds = pf_scene::Bounds::new(20.0, 20.0, 180.0, 48.0);
        let root_id = pf_scene::NodeId::new("occlusion-probe").unwrap();
        let root = Node::new(
            root_id.clone(),
            Role::Group,
            "",
            pf_scene::Bounds::new(0.0, 0.0, 240.0, 96.0),
            "--color-surface-canvas",
        )
        .with_children(vec![
            Node::new(
                pf_scene::NodeId::new("occluded-label").unwrap(),
                Role::Text,
                "Fully covered",
                bounds,
                "--color-surface-canvas",
            ),
            Node::new(
                pf_scene::NodeId::new("later-fill").unwrap(),
                Role::Group,
                "",
                bounds,
                "--color-surface-raised",
            ),
        ]);
        let scene = pf_scene::Scene::new(root, root_id).unwrap();
        let metrics = SurfaceMetrics {
            logical_width: 240.0,
            logical_height: 96.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };

        let failure =
            assert_raster_text_legible(&scene, metrics, pf_theme::Base::Dusk, 100).unwrap_err();
        assert!(
            failure.contains("occluded-label") && failure.contains("occluded-by-later-paint"),
            "unexpected guard verdict: {failure}"
        );
    }

    #[test]
    fn raster_ink_guard_rejects_a_nearly_occluded_text_sliver() {
        let bounds = pf_scene::Bounds::new(20.0, 20.0, 180.0, 48.0);
        let root_id = pf_scene::NodeId::new("sliver-occlusion-probe").unwrap();
        let root = Node::new(
            root_id.clone(),
            Role::Group,
            "",
            pf_scene::Bounds::new(0.0, 0.0, 240.0, 96.0),
            "--color-surface-canvas",
        )
        .with_children(vec![
            Node::new(
                pf_scene::NodeId::new("sliver-label").unwrap(),
                Role::Text,
                "Nearly covered",
                bounds,
                "--color-surface-canvas",
            ),
            Node::new(
                pf_scene::NodeId::new("later-rounded-fill").unwrap(),
                Role::Group,
                "",
                pf_scene::Bounds::new(24.0, 20.0, 176.0, 48.0),
                "--color-surface-raised",
            ),
        ]);
        let scene = pf_scene::Scene::new(root, root_id).unwrap();
        let metrics = SurfaceMetrics {
            logical_width: 240.0,
            logical_height: 96.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let failure =
            assert_raster_text_legible(&scene, metrics, pf_theme::Base::Dusk, 100).unwrap_err();
        assert!(failure.contains("sliver-label") && failure.contains("occluded-by-later-paint"));
    }

    struct TextScaleHost {
        inner: OffscreenHost,
        scales: Vec<f32>,
    }

    impl TextScaleHost {
        fn new() -> Self {
            Self {
                inner: OffscreenHost::new(SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Insets::default(),
                    orientation: Orientation::Landscape,
                }),
                scales: Vec::new(),
            }
        }
    }

    impl FrameHost for TextScaleHost {
        fn metrics(&self) -> SurfaceMetrics {
            self.inner.metrics()
        }

        fn present(&mut self, scene: &pf_scene::Scene) -> pf_ports::PresentResult {
            self.inner.present(scene)
        }
    }

    impl RenderedFrameHost for TextScaleHost {
        fn set_text_scale(&mut self, factor: f32) -> Result<(), String> {
            self.scales.push(factor);
            Ok(())
        }

        fn render_notes(&self) -> Option<&[RenderNote]> {
            None
        }

        fn raster_frame(&self) -> Option<&RasterFrame> {
            None
        }
    }

    #[test]
    fn renderer_receives_loaded_and_changed_text_scale() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        let loaded = EffectivePreference {
            key: PreferenceKey("textScale".into()),
            effective: PreferenceValue::Text("150%".into()),
            stored: PreferenceValue::Text("150%".into()),
            applied: true,
        };
        core.load_preferences(
            &FakePreferencePort::new([loaded], ChangeAuthority("user".into())),
            true,
        )
        .unwrap();
        let mut host = TextScaleHost::new();

        apply_text_scale(&mut host, &core).unwrap();
        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("textScale".into()),
            effective: PreferenceValue::Text("200%".into()),
            stored: PreferenceValue::Text("200%".into()),
            applied: true,
        });
        apply_text_scale(&mut host, &core).unwrap();

        assert_eq!(host.scales, [1.5, 2.0]);
    }

    #[test]
    fn library_and_settings_evidence_routes_render_without_notes() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };

        for (route, room_moves) in [("Library", 1), ("Settings", 2)] {
            let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
            for _ in 0..room_moves {
                core.action(&ShellAction::Custom("Room.next".into()));
            }
            let scene = core.scene(metrics, "").unwrap();
            let mut host = OffscreenHost::new(metrics);
            host.present(&scene).unwrap();
            assert_eq!(host.frame().unwrap().notes, [], "{route} render notes");
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn fixture_library_matches_the_full_mockup_roster_and_home_six() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        assert_eq!(snapshot.items.len(), 23);
        assert_eq!(
            snapshot
                .items
                .iter()
                .take(6)
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            [
                "Ridgeline",
                "Hollow Tides",
                "Sunwake",
                "Moth & Lantern",
                "Steam Link",
                "Tidelines"
            ]
        );
        assert!(
            snapshot.items[..4]
                .iter()
                .all(|item| item.tags.iter().any(|tag| tag.starts_with("playtime:")))
        );
        assert!(matches!(
            snapshot.items[4].variants[0].availability,
            pf_catalog::Availability::Ready
        ));

        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Insets::default(),
                    orientation: Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        for item in snapshot.items.iter().take(6) {
            let card = scene
                .root()
                .children
                .iter()
                .find(|node| node.id.as_str() == format!("item-{}", item.id))
                .unwrap();
            let art = card
                .children
                .iter()
                .find(|node| node.id.as_str() == format!("home-card-art-{}", item.id))
                .unwrap();
            assert_eq!(
                matches!(art.content, pf_scene::NodeContent::Image { .. }),
                !matches!(item.id.as_str(), "steam-link" | "tidelines"),
                "{} art source",
                item.id
            );
            assert!(
                card.children
                    .iter()
                    .all(|node| !node.id.as_str().starts_with("action-name-")),
                "{} must use its canvas caption as explicit action-name ink",
                item.id
            );
        }
        for (id, initial, kind) in [("steam-link", "S", "Stream"), ("tidelines", "T", "Web app")] {
            assert!(scene.root().children.iter().any(|card| {
                card.children.iter().any(|node| {
                    node.id.as_str() == format!("home-card-initial-plate-{id}")
                        && node.accessible_label == initial
                }) && card.children.iter().any(|node| {
                    node.id.as_str() == format!("home-card-plate-kind-{id}")
                        && node.accessible_label == kind
                })
            }));
        }
        let steam = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "item-steam-link")
            .unwrap();
        assert!(steam.children.iter().any(|node| {
            node.id.as_str() == "home-card-badge-steam-link" && node.accessible_label == "⌁ Network"
        }));
        assert!(
            steam
                .children
                .iter()
                .all(|node| !node.id.as_str().contains("reason"))
        );
        assert_eq!(
            scene
                .root()
                .children
                .iter()
                .find(|node| node.id.as_str() == "hero-status")
                .unwrap()
                .accessible_label,
            "● Ready · Game · Installed · 34 hours on the trail"
        );
    }

    #[test]
    fn catalog_and_fixture_paths_share_authored_and_hashed_cover_compositions() {
        fn composition(scene: &pf_scene::Scene, item_id: &str) -> Vec<(String, pf_scene::Bounds)> {
            scene
                .root()
                .children
                .iter()
                .find(|node| node.id.as_str() == format!("item-{item_id}"))
                .unwrap()
                .children
                .iter()
                .filter(|node| {
                    node.id.as_str().contains("-art-")
                        || node.id.as_str().contains("-scene-")
                        || node.id.as_str().contains("-motif-")
                })
                .map(|node| (node.style_token.clone(), node.bounds))
                .collect()
        }
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let fixture: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut catalog = fixture.clone();
        let catalog_root = tempfile::tempdir().unwrap();
        for item in &mut catalog.items {
            if let Some(reference) = item.presentation.icon_reference.clone() {
                let app_dir = catalog_root.path().join(&item.id);
                fs::create_dir_all(app_dir.join("art")).unwrap();
                fs::write(
                    app_dir.join("art/cover.png"),
                    vendored_art(&reference).unwrap(),
                )
                .unwrap();
                item.presentation.icon_reference = Some("art/cover.png".into());
                for variant in &mut item.variants {
                    variant.launch_target.descriptor_path = app_dir.join("app.toml");
                }
            }
            item.id = format!("installed-applications:{}", item.id);
        }
        let fixture_scene = fixture_core(&fixture, &pf_theme::flagship(), false)
            .scene(metrics, "")
            .unwrap();
        let catalog_scene = catalog_core(&catalog, &pf_theme::flagship(), false)
            .scene(metrics, "")
            .unwrap();
        for fixture_item in fixture.items.iter().take(6) {
            let catalog_id = format!("installed-applications:{}", fixture_item.id);
            assert_eq!(
                composition(&fixture_scene, &fixture_item.id),
                composition(&catalog_scene, &catalog_id),
                "{} must keep its authored composition through catalog namespacing",
                fixture_item.id
            );
        }

        let mut unknowns = fixture.clone();
        unknowns.items.truncate(2);
        unknowns.items[0].id = "installed-applications:unknown-alpha".into();
        unknowns.items[1].id = "installed-applications:unknown-beta".into();
        let unknown_scene = catalog_core(&unknowns, &pf_theme::flagship(), false)
            .scene(metrics, "")
            .unwrap();
        assert_ne!(
            composition(&unknown_scene, &unknowns.items[0].id),
            composition(&unknown_scene, &unknowns.items[1].id)
        );
    }

    #[test]
    fn safe_return_active_session_sends_exactly_one_request() {
        use std::os::unix::net::UnixListener;

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = pf_wire::read_frame(&mut stream).unwrap();
            let request =
                serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap();
            assert!(matches!(
                request,
                pf_session_authority::RpcRequest::SafeReturn
            ));
            let body = serde_json::to_vec(&pf_session_authority::RpcResponse::Ok).unwrap();
            pf_wire::write_frame(&mut stream, &body).unwrap();
        });
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.launch_result(&LaunchResult::Accepted {
            session_id: "session-1".into(),
        });
        let session = SessionClient::new("test", SocketTransport::connect(&socket));

        request_safe_return_if_active(&mut core, &session);

        server.join().unwrap();
        assert!(!core.authority_unavailable());
    }

    #[test]
    fn safe_return_transient_failure_preserves_session_and_retries() {
        use std::os::unix::net::UnixListener;

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut failed_stream, _) = listener.accept().unwrap();
            let body = pf_wire::read_frame(&mut failed_stream).unwrap();
            let request =
                serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap();
            assert!(matches!(
                request,
                pf_session_authority::RpcRequest::SafeReturn
            ));
            drop(failed_stream);

            let (mut succeeding_stream, _) = listener.accept().unwrap();
            let body = pf_wire::read_frame(&mut succeeding_stream).unwrap();
            let request =
                serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap();
            assert!(matches!(
                request,
                pf_session_authority::RpcRequest::SafeReturn
            ));
            let body = serde_json::to_vec(&pf_session_authority::RpcResponse::Ok).unwrap();
            pf_wire::write_frame(&mut succeeding_stream, &body).unwrap();
        });
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.launch_result(&LaunchResult::Accepted {
            session_id: "session-1".into(),
        });
        let session = SessionClient::new("test", SocketTransport::connect(&socket));

        request_safe_return_if_active(&mut core, &session);
        assert!(core.session_active());
        assert!(core.authority_unavailable());

        request_safe_return_if_active(&mut core, &session);
        server.join().unwrap();
        assert!(core.session_active());
        assert!(!core.authority_unavailable());

        core.session_event(&SessionEvent::Terminal(TerminalReceipt::Returned {
            session_id: "session-1".into(),
        }));
        assert!(!core.session_active());
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Returned);
    }

    #[test]
    fn safe_return_failure_survives_successful_events_poll_until_session_ends() {
        use std::os::unix::net::UnixListener;

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut failed_stream, _) = listener.accept().unwrap();
            let body = pf_wire::read_frame(&mut failed_stream).unwrap();
            assert!(matches!(
                serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap(),
                pf_session_authority::RpcRequest::SafeReturn
            ));
            drop(failed_stream);

            for expected in ["history", "events", "history"] {
                let (mut stream, _) = listener.accept().unwrap();
                let body = pf_wire::read_frame(&mut stream).unwrap();
                let request =
                    serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap();
                let response = match (expected, request) {
                    ("history", pf_session_authority::RpcRequest::History) => {
                        pf_session_authority::RpcResponse::History { entries: vec![] }
                    }
                    ("events", pf_session_authority::RpcRequest::Events { .. }) => {
                        pf_session_authority::RpcResponse::Events { events: vec![] }
                    }
                    (_, request) => panic!("unexpected {expected} request: {request:?}"),
                };
                pf_wire::write_frame(&mut stream, &serde_json::to_vec(&response).unwrap()).unwrap();
            }
        });
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.launch_result(&LaunchResult::Accepted {
            session_id: "session-1".into(),
        });
        let mut session = SessionClient::new("test", SocketTransport::connect(&socket));

        request_safe_return_if_active(&mut core, &session);
        drive_socket_session(&mut core, &mut session).unwrap();
        server.join().unwrap();
        assert!(core.authority_unavailable());
        assert_eq!(
            redraw_state(&core).3.as_deref(),
            Some("The session service isn't reachable; Safe Return can retry"),
            "the state used to decide and present the follow-up frame must keep the note"
        );

        core.session_event(&SessionEvent::Terminal(TerminalReceipt::Returned {
            session_id: "session-1".into(),
        }));
        assert!(!core.authority_unavailable());
        assert_eq!(core.session_status(), None);
    }

    #[test]
    fn safe_return_backend_gone_keeps_active_session_retryable() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.launch_result(&LaunchResult::Accepted {
            session_id: "session-1".into(),
        });
        let state = tempfile::tempdir().unwrap();
        let nonexistent_socket = state.path().join("authority.sock");
        let session = SessionClient::new("test", SocketTransport::connect(nonexistent_socket));

        request_safe_return_if_active(&mut core, &session);
        assert!(core.session_active());
        let first_revision = core.revision();

        request_safe_return_if_active(&mut core, &session);
        assert!(core.session_active());
        assert!(core.authority_unavailable());
        assert!(core.revision() > first_revision);
    }

    #[test]
    fn safe_return_at_home_sends_nothing() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        let state = tempfile::tempdir().unwrap();
        let nonexistent_socket = state.path().join("authority.sock");
        let session = SessionClient::new("test", SocketTransport::connect(nonexistent_socket));

        request_safe_return_if_active(&mut core, &session);

        assert!(!core.authority_unavailable());
    }

    fn unfinished_history(started: bool, ended: bool) -> pf_session_authority::HistoryEntry {
        pf_session_authority::HistoryEntry {
            session_id: "session-1".into(),
            item_id: "glass-harbor".into(),
            receipt: None,
            started_at: started.then(std::time::SystemTime::now),
            ended_at: ended.then(|| pf_session_authority::EndStamp {
                at: std::time::SystemTime::now(),
                precision: pf_session_authority::EndPrecision::Observed,
            }),
        }
    }

    #[test]
    fn supervisor_reconciles_an_already_running_authority() {
        let history = [unfinished_history(true, false)];

        assert_eq!(
            observable_phase_from_history(&history),
            ObservableAuthorityPhase::Running
        );
    }

    #[test]
    fn supervisor_recognizes_a_return_already_in_progress() {
        let history = [unfinished_history(true, true)];

        assert_eq!(
            observable_phase_from_history(&history),
            ObservableAuthorityPhase::Returning
        );
    }

    fn assert_supervisor_adopts_marker_appearing_during_reconcile(phase: ObservableAuthorityPhase) {
        use std::os::unix::net::UnixListener;
        use std::sync::{Arc, Mutex};

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let server_requests = Arc::clone(&requests);
        let startup_observations = usize::from(phase == ObservableAuthorityPhase::Starting);
        let server = thread::spawn(move || {
            for _ in 0..startup_observations + 5 {
                let (mut stream, _) = listener.accept().unwrap();
                let body = pf_wire::read_frame(&mut stream).unwrap();
                server_requests.lock().unwrap().push(
                    serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap(),
                );
                let body = serde_json::to_vec(&pf_session_authority::RpcResponse::Ok).unwrap();
                pf_wire::write_frame(&mut stream, &body).unwrap();
            }
        });

        let marker = state.path().join("sessions/session-1.running");
        let mut samples = [None, Some(marker.clone())].into_iter();
        let adopted = reconcile_desktop_sim_startup(
            &socket,
            phase,
            || Ok(phase),
            || {
                Ok(samples
                    .next()
                    .expect("startup sampled marker too many times"))
            },
        )
        .unwrap();
        assert_eq!(adopted, Some(marker));
        {
            let startup_requests = requests.lock().unwrap();
            assert_eq!(startup_requests.len(), startup_observations);
            if phase == ObservableAuthorityPhase::Starting {
                assert!(matches!(
                    startup_requests[0],
                    pf_session_authority::RpcRequest::Observe {
                        observation: pf_session_authority::RpcObservation::SessionRunning
                    }
                ));
            }
        }

        // A normally observed stop must still complete after the startup adoption.
        observe_desktop_sim_return(&socket, true).unwrap();
        server.join().unwrap();

        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), startup_observations + 5);
    }

    #[test]
    fn supervisor_adopts_marker_appearing_after_starting_sample() {
        assert_supervisor_adopts_marker_appearing_during_reconcile(
            ObservableAuthorityPhase::Starting,
        );
    }

    #[test]
    fn supervisor_adopts_marker_appearing_after_running_sample() {
        assert_supervisor_adopts_marker_appearing_during_reconcile(
            ObservableAuthorityPhase::Running,
        );
    }

    #[test]
    fn supervisor_rechecks_idle_phase_before_adopting_racing_marker() {
        use std::os::unix::net::UnixListener;

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = pf_wire::read_frame(&mut stream).unwrap();
            let request =
                serde_json::from_slice::<pf_session_authority::RpcRequest>(&body).unwrap();
            assert!(matches!(
                request,
                pf_session_authority::RpcRequest::Observe {
                    observation: pf_session_authority::RpcObservation::SessionRunning
                }
            ));
            let body = serde_json::to_vec(&pf_session_authority::RpcResponse::Ok).unwrap();
            pf_wire::write_frame(&mut stream, &body).unwrap();
        });

        let marker = state.path().join("session-1.running");
        fs::write(&marker, []).unwrap();
        let adopted = reconcile_desktop_sim_startup(
            &socket,
            ObservableAuthorityPhase::Idle,
            || Ok(ObservableAuthorityPhase::Starting),
            || Ok(Some(marker.clone())),
        )
        .unwrap();

        assert_eq!(adopted, Some(marker));
        server.join().unwrap();
    }

    #[test]
    fn supervisor_removes_idle_orphan_before_next_launch_cycle() {
        use std::os::unix::net::UnixListener;

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let orphan = state.path().join("orphan.running");
        fs::write(&orphan, []).unwrap();
        let adopted = reconcile_desktop_sim_startup(
            &socket,
            ObservableAuthorityPhase::Idle,
            || Ok(ObservableAuthorityPhase::Idle),
            || Ok(Some(orphan.clone())),
        )
        .unwrap();
        assert_eq!(adopted, None);
        assert!(!orphan.exists());

        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            for _ in 0..6 {
                let (mut stream, _) = listener.accept().unwrap();
                let _request = pf_wire::read_frame(&mut stream).unwrap();
                let body = serde_json::to_vec(&pf_session_authority::RpcResponse::Ok).unwrap();
                pf_wire::write_frame(&mut stream, &body).unwrap();
            }
        });
        let next_marker = state.path().join("next.running");
        fs::write(&next_marker, []).unwrap();
        let adopted = reconcile_desktop_sim_startup(
            &socket,
            ObservableAuthorityPhase::Starting,
            || Ok(ObservableAuthorityPhase::Starting),
            || Ok(Some(next_marker.clone())),
        )
        .unwrap();
        assert_eq!(adopted, Some(next_marker));
        observe_desktop_sim_return(&socket, true).unwrap();
        server.join().unwrap();
    }

    fn observe_against_response(response: pf_session_authority::RpcResponse) -> Result<(), String> {
        use std::os::unix::net::UnixListener;

        let state = tempfile::tempdir().unwrap();
        let socket = state.path().join("authority.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _request = pf_wire::read_frame(&mut stream).unwrap();
            let body = serde_json::to_vec(&response).unwrap();
            pf_wire::write_frame(&mut stream, &body).unwrap();
        });

        let result = observe_desktop_sim(
            &socket,
            pf_session_authority::RpcObservation::SessionRunning,
        );
        server.join().unwrap();
        result
    }

    #[test]
    fn duplicate_observation_is_benign() {
        assert!(
            observe_against_response(pf_session_authority::RpcResponse::Error {
                message: "InvalidObservation".into(),
            })
            .is_ok()
        );
    }

    #[test]
    fn malformed_observation_error_remains_fatal() {
        assert_eq!(
            observe_against_response(pf_session_authority::RpcResponse::Error {
                message: "MalformedObservation".into(),
            }),
            Err("MalformedObservation".into())
        );
    }

    #[cfg(feature = "wayland")]
    struct TestWaylandHost {
        closed: bool,
        events: VecDeque<KeyEvent>,
        repeat_info: RepeatInfo,
    }

    #[cfg(feature = "wayland")]
    impl WaylandInputHost for TestWaylandHost {
        fn is_closed(&self) -> bool {
            self.closed
        }

        fn repeat_info(&self) -> Option<RepeatInfo> {
            Some(self.repeat_info)
        }

        fn poll_key_event(&mut self) -> Option<KeyEvent> {
            self.events.pop_front()
        }
    }

    #[cfg(feature = "wayland")]
    fn effective_map() -> EffectiveMap {
        let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json")).unwrap();
        EffectiveMap::load(contract, &MemoryStore::default()).unwrap()
    }

    #[cfg(feature = "wayland")]
    fn key_event(code: u32, keysym: u32, state: KeyState, key: Key) -> KeyEvent {
        KeyEvent {
            code,
            keysym,
            state,
            key,
        }
    }

    #[cfg(feature = "wayland")]
    #[test]
    fn keyboard_mapping_table_only_exposes_effective_actions() {
        let map = effective_map();
        let cases = [
            (Key::Up, 0xff52, ShellAction::Move(pf_scene::AxisMove::Up)),
            (
                Key::Down,
                0xff54,
                ShellAction::Move(pf_scene::AxisMove::Down),
            ),
            (
                Key::Left,
                0xff51,
                ShellAction::Move(pf_scene::AxisMove::Left),
            ),
            (
                Key::Right,
                0xff53,
                ShellAction::Move(pf_scene::AxisMove::Right),
            ),
            (Key::Enter, 0xff0d, ShellAction::Activate),
            (Key::Char(' '), 0x20, ShellAction::Custom("Start".into())),
            (Key::Escape, 0xff1b, ShellAction::Back),
            (Key::Other(0xff08), 0xff08, ShellAction::Back),
            (
                Key::Other(0xff09),
                0xff09,
                ShellAction::Custom("Favorite".into()),
            ),
            (
                Key::Char('F'),
                u32::from('F'),
                ShellAction::Custom("Favorite".into()),
            ),
            (
                Key::Char('s'),
                u32::from('s'),
                ShellAction::Custom("SafeReturn".into()),
            ),
        ];
        for (key, keysym, expected) in cases {
            assert_eq!(effective_keyboard_action(&map, key, keysym), Some(expected));
        }
        assert_eq!(
            effective_keyboard_action(&map, Key::Char('q'), u32::from('q')),
            None
        );

        let mut without_quick = map.clone();
        let mappings = without_quick
            .mappings()
            .iter()
            .filter(|mapping| mapping.action != "Quick")
            .cloned()
            .collect::<Vec<_>>();
        // Build a map from a contract lacking Quick to prove keyboard affordances
        // cannot drift beyond the effective action set.
        let mut contract: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/device.json")).unwrap();
        contract["effective_map"] = serde_json::to_value(mappings).unwrap();
        let contract = DeviceContract::parse_json(&contract.to_string()).unwrap();
        without_quick = EffectiveMap::load(contract, &MemoryStore::default()).unwrap();
        assert_eq!(
            effective_keyboard_action(&without_quick, Key::Char('f'), u32::from('f')),
            None
        );

        let mut remapped_contract: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/device.json")).unwrap();
        for mapping in remapped_contract["effective_map"].as_array_mut().unwrap() {
            match mapping["action"].as_str().unwrap() {
                "Start" => mapping["binding"]["controls"] = serde_json::json!(["r1"]),
                "Move.right" => mapping["binding"]["controls"] = serde_json::json!(["start"]),
                _ => {}
            }
        }
        let remapped_contract = DeviceContract::parse_json(&remapped_contract.to_string()).unwrap();
        let remapped = EffectiveMap::load(remapped_contract, &MemoryStore::default()).unwrap();
        assert_eq!(
            effective_keyboard_action(&remapped, Key::Char(' '), 0x20),
            Some(ShellAction::Custom("Start".into()))
        );

        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        core.reset_first_run();
        let start = effective_keyboard_action(&map, Key::Char(' '), 0x20).unwrap();
        assert_eq!(core.action(&start), Some(Effect::CompleteFirstRun));
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Ready);
    }

    #[cfg(feature = "wayland")]
    #[test]
    fn repeat_scheduler_uses_fake_time_and_only_repeats_focus_moves() {
        let mut scheduler = KeyRepeatScheduler::default();
        scheduler.transition(
            1,
            true,
            Some(ShellAction::Move(pf_scene::AxisMove::Up)),
            Duration::ZERO,
            Duration::from_millis(300),
        );
        scheduler.transition(
            2,
            true,
            Some(ShellAction::Activate),
            Duration::ZERO,
            Duration::from_millis(300),
        );
        let interval = Duration::from_millis(100);
        assert!(
            scheduler
                .due(Duration::from_millis(299), interval)
                .is_empty()
        );
        assert_eq!(
            scheduler.due(Duration::from_millis(500), interval),
            vec![
                ShellAction::Move(pf_scene::AxisMove::Up),
                ShellAction::Move(pf_scene::AxisMove::Up),
                ShellAction::Move(pf_scene::AxisMove::Up),
            ]
        );
        scheduler.transition(
            1,
            false,
            None,
            Duration::from_millis(501),
            Duration::from_millis(300),
        );
        assert!(scheduler.due(Duration::from_secs(1), interval).is_empty());
    }

    #[test]
    fn evdev_repeat_defaults_hold_release_and_device_loss_are_deterministic() {
        let mut scheduler = KeyRepeatScheduler::default();
        scheduler.transition(
            103,
            true,
            Some(ShellAction::Move(pf_scene::AxisMove::Up)),
            Duration::ZERO,
            EVDEV_REPEAT_DELAY,
        );
        scheduler.transition(
            304,
            true,
            Some(ShellAction::Activate),
            Duration::ZERO,
            EVDEV_REPEAT_DELAY,
        );

        assert!(
            scheduler
                .due(Duration::from_millis(399), EVDEV_REPEAT_INTERVAL)
                .is_empty()
        );
        assert_eq!(
            scheduler.due(Duration::from_millis(560), EVDEV_REPEAT_INTERVAL),
            vec![
                ShellAction::Move(pf_scene::AxisMove::Up),
                ShellAction::Move(pf_scene::AxisMove::Up),
                ShellAction::Move(pf_scene::AxisMove::Up),
            ]
        );

        scheduler.transition(
            103,
            false,
            None,
            Duration::from_millis(561),
            EVDEV_REPEAT_DELAY,
        );
        assert!(
            scheduler
                .due(Duration::from_secs(2), EVDEV_REPEAT_INTERVAL)
                .is_empty()
        );

        scheduler.transition(
            108,
            true,
            Some(ShellAction::Move(pf_scene::AxisMove::Down)),
            Duration::from_secs(2),
            EVDEV_REPEAT_DELAY,
        );
        scheduler.clear();
        assert!(
            scheduler
                .due(Duration::from_secs(3), EVDEV_REPEAT_INTERVAL)
                .is_empty()
        );
    }

    #[cfg(feature = "wayland")]
    #[test]
    fn closed_wayland_host_yields_closed() {
        let mut input = WaylandInteractiveInput::new(effective_map());
        let mut host = TestWaylandHost {
            closed: true,
            events: VecDeque::new(),
            repeat_info: RepeatInfo {
                rate: 10,
                delay_ms: 300,
            },
        };

        assert_eq!(
            input
                .next_action(&mut host, Deadline(MonotonicTime::ZERO))
                .unwrap(),
            ActionPoll::Closed
        );

        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let Effect::Launch(_) = core.action(&ShellAction::Activate).unwrap() else {
            panic!("ready fixture must launch");
        };
        core.launch_result(&LaunchResult::Accepted {
            session_id: "closed-session".into(),
        });
        core.session_event(&SessionEvent::Observed(ObservedSessionState::Running));
        let mut frame_host = OffscreenHost::new(SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        });
        assert!(!present_interactive(&mut frame_host, &mut core, "A Open").unwrap());
    }

    #[cfg(feature = "wayland")]
    #[test]
    fn synthetic_release_clears_wayland_direction_repeat() {
        let info = RepeatInfo {
            rate: 10,
            delay_ms: 300,
        };
        let mut input = WaylandInteractiveInput::new(effective_map());
        let mut host = TestWaylandHost {
            closed: false,
            events: VecDeque::from([key_event(1, 0xff52, KeyState::Pressed, Key::Up)]),
            repeat_info: info,
        };

        assert!(matches!(
            input
                .next_action(&mut host, Deadline(MonotonicTime::ZERO))
                .unwrap(),
            ActionPoll::Event(ActionEvent::Action(ShellAction::Move(
                pf_scene::AxisMove::Up
            )))
        ));

        // Keyboard leave/seat loss/reconnect releases have the same KeyEvent shape as
        // physical releases, so this must travel through the normal transition path.
        host.events
            .push_back(key_event(1, 0xff52, KeyState::Released, Key::Up));
        input.started = Instant::now()
            .checked_sub(Duration::from_secs(1))
            .expect("one second before now should be representable");
        assert_eq!(
            input
                .next_action(&mut host, Deadline(MonotonicTime::ZERO))
                .unwrap(),
            ActionPoll::DeadlineReached
        );
        assert!(
            input
                .repeat
                .due(Duration::from_secs(2), Duration::from_millis(100))
                .is_empty()
        );
    }

    #[test]
    fn missing_catalog_root_degrades_but_existing_unreadable_root_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing");
        let missing_provider = installed_app_provider(&missing, dir.path().join("favorites.json"));
        assert!(
            catalog_snapshot(&missing_provider, &missing)
                .unwrap()
                .items
                .is_empty()
        );

        let not_a_directory = dir.path().join("catalog-file");
        fs::write(&not_a_directory, b"not a directory").unwrap();
        let unreadable_provider =
            installed_app_provider(&not_a_directory, dir.path().join("other-favorites.json"));
        assert!(catalog_snapshot(&unreadable_provider, &not_a_directory).is_err());
    }

    fn scanned_manifest_with_runtime(id: &str, family: &str, abi: &str) -> String {
        format!(
            r#"[app]
id="{id}"
name="Catalog Art"
category="game"
icon="art/cover.png"
version="1.0.0"
[runtime]
family="{family}"
abi="{abi}"
[launch]
exec="./launch"
"#
        )
    }

    fn scanned_manifest() -> String {
        scanned_manifest_with_runtime("com.example.art", RUNTIME_FAMILY, RUNTIME_ABI)
    }

    struct FailingScreenshotWriter;

    impl ScreenshotWriter for FailingScreenshotWriter {
        fn write_png(&self, _path: &Path, _frame: &RasterFrame) -> Result<(), String> {
            Err("injected write failure".into())
        }
    }

    struct FlushFailingWriter;

    impl Write for FlushFailingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("injected flush failure"))
        }
    }

    struct UnavailableSession;

    struct AlwaysConflictingFavorites {
        snapshot: CatalogSnapshot,
    }

    #[test]
    fn catalog_art_resolver_reads_relative_file() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("art")).unwrap();
        fs::write(dir.path().join("art/cover.png"), b"cover bytes").unwrap();

        assert_eq!(
            read_catalog_art(dir.path(), "art/cover.png").as_deref(),
            Some(&b"cover bytes"[..])
        );
    }

    #[test]
    fn catalog_art_resolver_returns_none_for_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_catalog_art(dir.path(), "art/missing.png").is_none());
    }

    #[test]
    fn catalog_art_resolver_rejects_oversized_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.png");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_CATALOG_ART_BYTES + 1).unwrap();

        assert!(read_catalog_art(dir.path(), "large.png").is_none());
    }

    fn rendered_home(mut core: ShellCore) -> Vec<u8> {
        core.authority_snapshot(false);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let mut host = OffscreenHost::new(metrics);
        present(&mut host, &mut core, "A Open").unwrap();
        host.frame().unwrap().rgba.clone()
    }

    #[test]
    fn snapshot_relative_art_renders_pixels_from_beside_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let snapshot_path = dir.path().join("catalog.json");
        let mut snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        snapshot.items.truncate(1);
        snapshot.items[0].presentation.icon_reference = Some("art/cover.png".into());
        fs::create_dir(dir.path().join("art")).unwrap();
        fs::write(
            dir.path().join("art/cover.png"),
            include_bytes!("../fixtures/art/hollow-tides.png"),
        )
        .unwrap();

        let actual = rendered_home(snapshot_core(
            &snapshot,
            &snapshot_path,
            &pf_theme::flagship(),
            false,
        ));
        snapshot.items[0].presentation.icon_reference = Some("fixture-art:hollow-tides.png".into());
        let expected = rendered_home(fixture_core(&snapshot, &pf_theme::flagship(), false));

        assert_eq!(
            actual, expected,
            "snapshot must render the adjacent file's pixels"
        );
    }

    #[test]
    fn installed_relative_art_cannot_be_shadowed_by_vendored_fixture_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("apps");
        let app = root.join("example");
        fs::create_dir_all(app.join("art")).unwrap();
        fs::write(
            app.join("app.toml"),
            scanned_manifest().replace("art/cover.png", "art/ridgeline.png"),
        )
        .unwrap();
        fs::write(
            app.join("art/ridgeline.png"),
            include_bytes!("../fixtures/art/hollow-tides.png"),
        )
        .unwrap();
        let mut snapshot = installed_app_provider(&root, dir.path().join("favorites.json"))
            .snapshot()
            .unwrap();

        let actual = rendered_home(catalog_core(&snapshot, &pf_theme::flagship(), false));
        snapshot.items[0].presentation.icon_reference = Some("fixture-art:hollow-tides.png".into());
        let expected = rendered_home(fixture_core(&snapshot, &pf_theme::flagship(), false));

        assert_eq!(
            actual, expected,
            "manifest-relative art must win by item path"
        );
    }

    #[test]
    fn shipped_fixture_catalog_resolves_every_namespaced_cover() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let core = fixture_core(&snapshot, &pf_theme::flagship(), false);

        for item in snapshot
            .items
            .iter()
            .filter(|item| item.presentation.icon_reference.is_some())
        {
            let reference = item.presentation.icon_reference.as_deref().unwrap();
            assert!(reference.starts_with("fixture-art:"));
            assert!(
                vendored_art(reference).is_some(),
                "missing vendored art for {reference}"
            );
            assert_eq!(
                core.art_treatment(&item.id),
                Some(pf_shell_core::ArtTreatment::CatalogArt)
            );
        }
    }

    #[test]
    fn missing_snapshot_art_keeps_plate_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        snapshot.items.truncate(1);
        snapshot.items[0].presentation.icon_reference = Some("art/missing.png".into());

        let core = snapshot_core(
            &snapshot,
            &dir.path().join("catalog.json"),
            &pf_theme::flagship(),
            false,
        );

        assert!(matches!(
            core.art_treatment("ridgeline"),
            Some(pf_shell_core::ArtTreatment::EditionPlate { .. })
        ));
    }

    #[test]
    fn production_catalog_provider_matches_canonical_runtime_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("apps");
        let ready_app = root.join("ready");
        let incompatible_app = root.join("incompatible");
        fs::create_dir_all(&ready_app).unwrap();
        fs::create_dir_all(&incompatible_app).unwrap();
        fs::write(
            ready_app.join("app.toml"),
            scanned_manifest_with_runtime("com.example.ready", "pocketforge/native", "1"),
        )
        .unwrap();
        fs::write(
            incompatible_app.join("app.toml"),
            scanned_manifest_with_runtime("com.example.incompatible", "pocketforge/other", "2"),
        )
        .unwrap();

        let snapshot = installed_app_provider(&root, dir.path().join("favorites.json"))
            .snapshot()
            .unwrap();
        let availability = |id: &str| {
            &snapshot
                .items
                .iter()
                .take(6)
                .find(|item| item.id.ends_with(id))
                .unwrap()
                .variants[0]
                .availability
        };

        assert!(matches!(
            availability("com.example.ready"),
            pf_catalog::Availability::Ready
        ));
        assert!(matches!(
            availability("com.example.incompatible"),
            pf_catalog::Availability::IncompatibleRuntime { .. }
        ));
    }

    #[test]
    fn scanned_catalog_art_reaches_shell_boot() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("apps");
        let app = root.join("example");
        fs::create_dir_all(app.join("art")).unwrap();
        fs::write(app.join("app.toml"), scanned_manifest()).unwrap();
        fs::write(app.join("art/cover.png"), b"catalog cover bytes").unwrap();
        let snapshot = installed_app_provider(&root, dir.path().join("favorites.json"))
            .snapshot()
            .unwrap();

        let core = catalog_core(&snapshot, &pf_theme::flagship(), false);

        assert_eq!(
            snapshot.items[0].presentation.icon_reference.as_deref(),
            Some("art/cover.png")
        );
        assert_eq!(
            core.art_treatment("installed-applications:com.example.art"),
            Some(pf_shell_core::ArtTreatment::CatalogArt)
        );
    }

    #[test]
    fn unavailable_device_ports_render_disabled_honest_rooms() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        let mut network = UnavailableNetworkPort;
        let time = UnavailableTimePort;
        let transfer = UnavailableTransferPort;
        core.load_network(&mut network);
        core.load_system(&time, &transfer);
        core.authority_snapshot(false);
        core.action(&ShellAction::Custom("Room.next".into()));
        core.action(&ShellAction::Custom("Room.next".into()));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };

        let network_scene = format!("{:?}", core.scene(metrics, "").unwrap());
        assert!(!network_scene.contains("settings-nav-network"));
        assert!(network_scene.contains("settings-nav-system"));
        assert_eq!(core.action(&ShellAction::Activate), None);

        assert!(network_scene.contains("Button remap"));
    }

    #[test]
    fn screenshot_is_a_decodable_frame_sized_png() {
        let state = tempfile::tempdir().unwrap();
        let frame = RasterFrame {
            width: 3,
            height: 2,
            rgba: vec![0x7f; 3 * 2 * 4],
            damage: None,
            notes: Vec::new(),
        };

        let path = capture_screenshot(&frame, state.path(), &FsScreenshotWriter).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(fs::File::open(path).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!(reader.info().width, frame.width);
        assert_eq!(reader.info().height, frame.height);
    }

    #[test]
    fn screenshot_writer_failure_drives_failure_toast() {
        let state = tempfile::tempdir().unwrap();
        let frame = RasterFrame {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            damage: None,
            notes: Vec::new(),
        };
        let mut core = fixture_core(
            &serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap(),
            &pf_theme::flagship(),
            false,
        );

        let result = capture_screenshot(&frame, state.path(), &FailingScreenshotWriter);
        core.screenshot_result(result.as_ref().map(|_| "unused").map_err(|_| ()));

        assert!(result.is_err());
        assert_eq!(core.session_status(), Some("Screenshot could not be saved"));
    }

    #[test]
    fn screenshot_flush_failure_drives_failure_toast() {
        let frame = RasterFrame {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            damage: None,
            notes: Vec::new(),
        };
        let mut core = fixture_core(
            &serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap(),
            &pf_theme::flagship(),
            false,
        );

        let result = write_png(FlushFailingWriter, &frame);
        core.screenshot_result(result.as_ref().map(|()| "unused").map_err(|_| ()));

        assert!(result.is_err());
        assert_eq!(core.session_status(), Some("Screenshot could not be saved"));
    }

    #[test]
    fn screenshot_rename_failure_drives_failure_toast() {
        let state = tempfile::tempdir().unwrap();
        let final_path = state.path().join("already-a-directory.png");
        fs::create_dir(&final_path).unwrap();
        let frame = RasterFrame {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            damage: None,
            notes: Vec::new(),
        };
        let mut core = fixture_core(
            &serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap(),
            &pf_theme::flagship(),
            false,
        );

        let result = FsScreenshotWriter.write_png(&final_path, &frame);
        core.screenshot_result(result.as_ref().map(|()| "unused").map_err(|_| ()));

        assert!(result.is_err());
        assert_eq!(core.session_status(), Some("Screenshot could not be saved"));
        assert_eq!(fs::read_dir(state.path()).unwrap().count(), 1);
    }

    #[test]
    fn fbdev_and_settings_evidence_are_rejected_as_conflicting() {
        let args = vec!["--fbdev".into(), "--settings-evidence".into()];

        let error = validate_args(&args).unwrap_err();

        assert!(error.contains("--fbdev"));
        assert!(error.contains("--settings-evidence"));
    }

    #[test]
    fn catalog_snapshot_flag_is_exact_and_conflicts_with_catalog_root() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("catalog.json");
        fs::write(&path, include_bytes!("../fixtures/catalog.json")).unwrap();
        let snapshot = load_catalog_snapshot(&path).unwrap();
        assert_eq!(
            snapshot
                .items
                .iter()
                .take(6)
                .map(|item| item.title.as_str())
                .collect::<Vec<_>>(),
            [
                "Ridgeline",
                "Hollow Tides",
                "Sunwake",
                "Moth & Lantern",
                "Steam Link",
                "Tidelines"
            ]
        );
        assert_eq!(
            snapshot.items[0]
                .tags
                .iter()
                .find(|tag| tag.starts_with("playtime:"))
                .map(String::as_str),
            Some("playtime:34 hours on the trail")
        );
        let args = vec![
            "--catalog-root".into(),
            "/path/that/must/not/be/scanned".into(),
            "--catalog-snapshot".into(),
            path.display().to_string(),
        ];
        let error = validate_args(&args).unwrap_err();
        assert!(error.contains("cannot be used together"));
    }

    #[cfg(not(feature = "wayland"))]
    #[test]
    fn wayland_flag_requires_wayland_feature() {
        let error = validate_args(&["--wayland".into()]).unwrap_err();

        assert!(error.contains("requires a build with the 'wayland' feature"));
    }

    impl FavoriteCatalog for AlwaysConflictingFavorites {
        fn snapshot(&self) -> Result<CatalogSnapshot, String> {
            Ok(self.snapshot.clone())
        }

        fn set_favorite(
            &self,
            _id: &str,
            _value: bool,
            _expected: pf_catalog::CatalogRevision,
        ) -> Result<pf_catalog::FavoriteCommitResult, String> {
            Ok(pf_catalog::FavoriteCommitResult::RevisionConflict {
                current: self.snapshot.revision,
            })
        }

        fn set_pinned_variant(
            &self,
            _item_id: &str,
            _variant_id: Option<&str>,
            _expected: pf_catalog::CatalogRevision,
        ) -> Result<pf_catalog::VariantPinCommitResult, String> {
            Ok(pf_catalog::VariantPinCommitResult::RevisionConflict {
                current: self.snapshot.revision,
            })
        }
    }

    impl SessionPort for UnavailableSession {
        fn launch(
            &mut self,
            _request: pf_ports::LaunchRequest,
        ) -> Result<LaunchResult, SessionError> {
            Err(SessionError::BackendUnavailable)
        }

        fn next_event(&mut self, _deadline: Deadline) -> Result<SessionPoll, SessionError> {
            Err(SessionError::BackendUnavailable)
        }

        fn history(&self) -> &[SessionEvent] {
            &[]
        }
    }

    #[test]
    fn favorite_toggle_success_changes_home_redraw_key() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let Effect::ToggleFavorite { item_id, favorite } = core
            .action(&ShellAction::Custom("Favorite".into()))
            .expect("home title supports favorite toggle")
        else {
            panic!("favorite action must emit a toggle effect");
        };
        let before_commit = redraw_state(&core);

        core.favorite_committed(&item_id, favorite);

        assert_ne!(before_commit, redraw_state(&core));
        assert_eq!(core.is_favorite(&item_id), favorite);
    }

    #[test]
    fn favorite_toggle_cas_failure_changes_redraw_key_for_toast() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let catalog = AlwaysConflictingFavorites {
            snapshot: snapshot.clone(),
        };
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let Effect::ToggleFavorite { item_id, favorite } = core
            .action(&ShellAction::Custom("Favorite".into()))
            .expect("home title supports favorite toggle")
        else {
            panic!("favorite action must emit a toggle effect");
        };
        let before_failure = redraw_state(&core);

        let status = commit_favorite(&catalog, &item_id, favorite).unwrap_err();
        core.favorite_failed(status);

        assert_ne!(before_failure, redraw_state(&core));
        assert_eq!(
            core.session_status(),
            Some("Favorites changed elsewhere; try again")
        );
    }

    #[test]
    fn chooser_favorite_action_does_not_write_variant_memory() {
        let mut snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let item = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == "moth-and-lantern")
            .unwrap();
        let mut second = item
            .variants
            .iter()
            .find(|variant| matches!(variant.availability, pf_catalog::Availability::Ready))
            .unwrap()
            .clone();
        second.id = "handheld".into();
        item.variants.push(second);
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Activate);
        let Effect::ToggleFavorite { item_id, favorite } = core
            .action(&ShellAction::Custom("Favorite".into()))
            .expect("chooser keeps the ordinary item favorite affordance")
        else {
            panic!("chooser must not emit a variant-memory effect");
        };
        assert_eq!(item_id, "moth-and-lantern");
        assert!(!favorite);
        assert!(snapshot.user_projection.pinned_variant_ids.is_empty());
    }

    #[test]
    fn unavailable_transport_is_boot_degraded_and_activation_is_honest() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let mut session = UnavailableSession;

        assert_eq!(
            wait_for_session_authority(&mut session, Duration::ZERO),
            Err(SessionError::BackendUnavailable)
        );
        core.session_backend_unavailable_at_boot();
        assert!(core.authority_unavailable());
        assert!(!core.recovery_available());
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Ready);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let boot_scene = core.scene(metrics, "").unwrap();
        assert!(boot_scene.root().children.iter().any(|node| {
            node.id.as_str() == "status-cluster" && node.accessible_label.contains('!')
        }));
        assert!(
            !boot_scene
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "session-status")
        );
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        let settings = core.scene(metrics, "").unwrap();
        assert!(
            !settings
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "settings-recovery")
        );
        core.action(&ShellAction::Move(pf_scene::AxisMove::Left));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Left));

        let Effect::Launch(request) = core.action(&ShellAction::Activate).unwrap() else {
            panic!("ready fixture must launch");
        };
        let before = redraw_state(&core);
        assert_eq!(
            session.launch(request),
            Err(SessionError::BackendUnavailable)
        );
        core.session_backend_unavailable();
        assert_ne!(before, redraw_state(&core));
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Ready);
        let scene = core.scene(metrics, "").unwrap();
        let status = scene
            .root()
            .children
            .iter()
            .find(|node| node.id.as_str() == "session-status")
            .unwrap();
        assert_eq!(
            status.accessible_label,
            "The session service isn't reachable"
        );
        let mut host = OffscreenHost::new(metrics);
        present(&mut host, &mut core, "A Open").unwrap();
        assert!(host.frame().is_some());
    }

    #[test]
    fn returned_receipt_after_idle_is_consumed_without_an_action() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let Effect::Launch(request) = core.action(&ShellAction::Activate).unwrap() else {
            panic!("ready fixture must launch");
        };
        let mut session = pf_ports::FakeSession::new(
            Ok(LaunchResult::Accepted {
                session_id: "late-session".into(),
            }),
            [
                pf_ports::ScriptedSession::Idle,
                pf_ports::ScriptedSession::Event(SessionEvent::Observed(
                    ObservedSessionState::Running,
                )),
                pf_ports::ScriptedSession::Event(SessionEvent::Observed(
                    ObservedSessionState::ObservationComplete,
                )),
                pf_ports::ScriptedSession::Event(SessionEvent::Terminal(
                    TerminalReceipt::Returned {
                        session_id: "late-session".into(),
                    },
                )),
                pf_ports::ScriptedSession::Idle,
            ],
        );
        core.launch_result(&session.launch(request).unwrap());

        core.drive_session(&mut session).unwrap();
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Starting);
        let before = redraw_state(&core);

        // This is the next idle-loop cadence: there is deliberately no action.
        core.drive_session(&mut session).unwrap();
        assert_ne!(before, redraw_state(&core));
        assert_eq!(core.presentation(), &pf_shell_core::Presentation::Returned);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let scene = core.scene(metrics, "").unwrap();
        assert!(scene.root().children.iter().any(|node| {
            node.id.as_str() == "route-heading" && node.accessible_label == "RECENT · JUST NOW"
        }));
        let mut host = OffscreenHost::new(metrics);
        present(&mut host, &mut core, "A Open").unwrap();
        assert!(host.frame().is_some());
    }

    #[test]
    fn interactive_session_gap_skips_present_but_safe_return_and_receipt_continue() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let mut host = OffscreenHost::new(metrics);
        assert!(present_interactive(&mut host, &mut core, "A Open").unwrap());
        let last_frame = host.frame().unwrap().rgba.clone();

        let Effect::Launch(request) = core.action(&ShellAction::Activate).unwrap() else {
            panic!("ready fixture must launch");
        };
        let mut session = pf_ports::FakeSession::new(
            Ok(LaunchResult::Accepted {
                session_id: "interactive-session".into(),
            }),
            [
                pf_ports::ScriptedSession::Event(SessionEvent::Observed(
                    ObservedSessionState::Running,
                )),
                pf_ports::ScriptedSession::Idle,
                pf_ports::ScriptedSession::Event(SessionEvent::Observed(
                    ObservedSessionState::ObservationComplete,
                )),
                pf_ports::ScriptedSession::Event(SessionEvent::Terminal(
                    TerminalReceipt::Returned {
                        session_id: "interactive-session".into(),
                    },
                )),
                pf_ports::ScriptedSession::Idle,
            ],
        );
        core.launch_result(&session.launch(request).unwrap());
        core.drive_session(&mut session).unwrap();

        assert!(!present_interactive(&mut host, &mut core, "A Open").unwrap());
        assert_eq!(host.frame().unwrap().rgba, last_frame);
        assert_eq!(
            core.action(&ShellAction::Custom("SafeReturn".into())),
            Some(Effect::SafeReturn),
            "synthetic S must still reach the safe-return submission path"
        );

        let in_session_revision = core.revision();
        core.drive_session(&mut session).unwrap();
        assert!(core.revision() > in_session_revision);
        assert!(present_interactive(&mut host, &mut core, "A Open").unwrap());
        assert!(
            core.scene(host.metrics(), "A Open")
                .unwrap()
                .root()
                .children
                .iter()
                .any(|node| node.id.as_str() == "route-heading"
                    && node.accessible_label == "RECENT · JUST NOW")
        );
    }

    #[test]
    fn corrupt_fixture_render_note_replaces_image_with_plate_on_redraw() {
        let mut snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        snapshot.items[0].presentation.icon_reference = Some("fixture-art:corrupt.png".into());
        snapshot.items[0].presentation.icon_decodable = true;
        let theme = pf_theme::flagship();
        let mut core = fixture_core(&snapshot, &theme, false);
        core.authority_snapshot(false);
        assert_eq!(
            core.art_treatment("ridgeline"),
            Some(pf_shell_core::ArtTreatment::CatalogArt)
        );
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let mut host = OffscreenHost::new(metrics);
        // Exercise the same presentation entry point used after an interactive action,
        // rather than relying on the launcher's initial-frame call.
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        present(&mut host, &mut core, "A Open").unwrap();
        assert!(matches!(
            core.art_treatment("ridgeline"),
            Some(pf_shell_core::ArtTreatment::EditionPlate { .. })
        ));
        assert!(host.frame().unwrap().notes.is_empty());
    }

    #[test]
    fn high_contrast_preference_changes_rendered_palette_in_same_redraw() {
        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        let metrics = SurfaceMetrics {
            logical_width: 1280.0,
            logical_height: 720.0,
            scale: 1.0,
            safe_insets: Insets::default(),
            orientation: Orientation::Landscape,
        };
        let mut host = OffscreenHost::new(metrics);
        let palette_probe = || {
            let root = pf_scene::Node::new(
                pf_scene::NodeId::new("palette-probe").unwrap(),
                pf_scene::Role::Text,
                "Palette",
                pf_scene::Bounds::new(16.0, 16.0, 160.0, 48.0),
                "--color-surface-canvas",
            );
            pf_scene::Scene::new(root, pf_scene::NodeId::new("palette-probe").unwrap()).unwrap()
        };

        present(&mut host, &mut core, "A Open").unwrap();
        host.present(&palette_probe()).unwrap();
        let standard = host.frame().unwrap();
        assert_eq!(&standard.rgba[..3], &[23, 21, 18]);

        core.preference_changed(&EffectivePreference {
            key: PreferenceKey("highContrast".into()),
            effective: PreferenceValue::Bool(true),
            stored: PreferenceValue::Bool(true),
            applied: true,
        });
        present(&mut host, &mut core, "A Open").unwrap();
        host.present(&palette_probe()).unwrap();
        let high_contrast = host.frame().unwrap();
        assert_eq!(&high_contrast.rgba[..3], &[0, 0, 0]);
        assert!(
            high_contrast
                .rgba
                .chunks_exact(4)
                .any(|pixel| pixel[..3] == [255, 255, 255]),
            "high-contrast label text should contain a white pixel"
        );
    }

    #[test]
    fn appearance_preference_is_durable_and_publishes_applied_change() {
        let dir = tempfile::tempdir().unwrap();
        let mut preferences = DurablePreferences::open(dir.path()).unwrap();
        assert_eq!(
            preferences
                .submit_change(PreferenceChange {
                    key: PreferenceKey("appearance".into()),
                    value: PreferenceValue::Text("Day".into()),
                    authority: ChangeAuthority("user".into()),
                })
                .unwrap(),
            PreferenceChangeResult::Accepted
        );
        let PreferencePoll::Changed(change) = preferences
            .next_change(Deadline(MonotonicTime::ZERO))
            .unwrap()
        else {
            panic!("appearance change must redraw immediately")
        };
        assert_eq!(change.effective, PreferenceValue::Text("Day".into()));
        let reopened = DurablePreferences::open(dir.path()).unwrap();
        assert_eq!(
            reopened
                .read(&PreferenceKey("appearance".into()))
                .unwrap()
                .unwrap()
                .effective,
            PreferenceValue::Text("Day".into())
        );
    }

    #[test]
    fn fresh_and_schema_v2_preferences_default_text_scale_to_one_hundred_percent() {
        for existing in [None, Some(r#"{"schemaVersion":2}"#)] {
            let dir = tempfile::tempdir().unwrap();
            if let Some(document) = existing {
                std::fs::write(dir.path().join("prefs.json"), document).unwrap();
            }
            let preferences = DurablePreferences::open(dir.path()).unwrap();
            assert_eq!(
                preferences
                    .read(&PreferenceKey("textScale".into()))
                    .unwrap()
                    .unwrap()
                    .effective,
                PreferenceValue::Text("100%".into())
            );
        }
    }

    #[test]
    fn fresh_preferences_and_first_run_render_at_one_hundred_percent() {
        let dir = tempfile::tempdir().unwrap();
        let preferences = DurablePreferences::open(dir.path()).unwrap();
        let observed = preferences
            .read(&PreferenceKey("textScale".into()))
            .unwrap()
            .unwrap();
        assert_eq!(observed.effective, PreferenceValue::Text("100%".into()));

        let snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.load_preferences(&preferences, false).unwrap();
        core.reset_first_run();
        let scene = core
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.0,
                    logical_height: 720.0,
                    scale: 1.0,
                    safe_insets: Insets::default(),
                    orientation: Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        assert!(format!("{scene:?}").contains("100%"));
        assert!(!format!("{scene:?}").contains("Current value 150%"));
    }

    #[test]
    fn first_run_completion_survives_two_boots_in_one_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = DurablePreferences::open(dir.path()).unwrap();
        assert!(!first.first_run_complete().unwrap());
        assert_eq!(
            first
                .submit_change(PreferenceChange {
                    key: PreferenceKey("firstRunComplete".into()),
                    value: PreferenceValue::Bool(true),
                    authority: ChangeAuthority("user".into()),
                })
                .unwrap(),
            PreferenceChangeResult::Accepted
        );
        drop(first);
        let second = DurablePreferences::open(dir.path()).unwrap();
        assert!(second.first_run_complete().unwrap());
    }

    #[test]
    fn remap_commit_survives_rebuilding_from_the_same_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remaps.json");
        let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json")).unwrap();
        let map = load_durable_map_or_shipped(contract.clone(), &path).unwrap();
        let mut remap = GamepadRemap::with_store(map, JsonRemapStore::at(&path));
        remap
            .preview("global", "Activate", pf_input_map::Binding::single("north"))
            .unwrap();
        remap.gamepad_action(&ShellAction::Activate).unwrap();
        drop(remap);

        let reloaded = load_durable_map_or_shipped(contract, &path).unwrap();
        assert!(
            control_bindings(&reloaded)
                .iter()
                .any(|binding| { binding.action == "Activate" && binding.binding == "Y" })
        );
    }

    #[test]
    fn corrupt_remap_file_recovers_store_for_commit_and_reset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remaps.json");
        fs::write(&path, b"not json").unwrap();
        let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json")).unwrap();

        let map = load_durable_map_or_shipped(contract.clone(), &path).unwrap();

        assert!(
            control_bindings(&map)
                .iter()
                .any(|binding| { binding.action == "Activate" && binding.binding == "A" })
        );
        let quarantine = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|entry| {
                entry
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("remaps.json.corrupt-")
            })
            .unwrap();
        assert_eq!(fs::read(quarantine).unwrap(), b"not json");

        let mut remap = GamepadRemap::with_store(map, JsonRemapStore::at(&path));
        remap
            .preview("global", "Activate", pf_input_map::Binding::single("north"))
            .unwrap();
        remap.gamepad_action(&ShellAction::Activate).unwrap();
        let committed = load_durable_map_or_shipped(contract.clone(), &path).unwrap();
        assert!(
            control_bindings(&committed)
                .iter()
                .any(|binding| binding.action == "Activate" && binding.binding == "Y")
        );

        remap.reset_defaults().unwrap();
        let reset = load_durable_map_or_shipped(contract, &path).unwrap();
        assert!(
            control_bindings(&reset)
                .iter()
                .any(|binding| binding.action == "Activate" && binding.binding == "A")
        );
    }
}
