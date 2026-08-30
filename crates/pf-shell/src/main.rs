use pf_catalog::{CatalogSnapshot, InstalledAppProvider};
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
use pf_render::{Palette, RasterFrame, RenderNote};
use pf_scene::{Insets, Orientation, SurfaceMetrics};
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
const HELP: &str = "pf-shell modes:\n  --wayland                 interactive desktop window\n  --fbdev                   interactive framebuffer\n  --desktop-sim-script      headless launch/return proof against session authority\n  --desktop-sim-supervise   observe desktop-sim marker lifecycle\n  --sim-frame               write one framebuffer fixture\n  --settings-evidence       write fixture PNGs\n\nWayland keyboard (only actions present in the effective input map are enabled):\n  Arrows   Move focus\n  Enter    Activate\n  Space    Start / continue\n  Escape, Backspace  Back\n  Tab, F   Quick / toggle favorite\n  S        Safe return\n";

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
        (Key::Enter, _) => "Activate",
        (_, 0x20) => "Start",
        (Key::Escape, _) | (_, 0xff08) => "Back",
        (_, 0xff09) | (Key::Char('f' | 'F'), _) => "Quick",
        (Key::Char('s' | 'S'), _) => "SafeReturn",
        _ => return None,
    };
    map.mappings()
        .iter()
        .any(|mapping| mapping.action == action)
        .then(|| match action {
            "Move.up" => ShellAction::Move(pf_scene::AxisMove::Up),
            "Move.down" => ShellAction::Move(pf_scene::AxisMove::Down),
            "Move.left" => ShellAction::Move(pf_scene::AxisMove::Left),
            "Move.right" => ShellAction::Move(pf_scene::AxisMove::Right),
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

fn fixture_art(reference: &str) -> Option<Arc<[u8]>> {
    match reference {
        "art/ridgeline.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/ridgeline.png")[..],
        )),
        "art/corrupt.png" => Some(Arc::from(
            &include_bytes!("../fixtures/art/corrupt.png")[..],
        )),
        _ => None,
    }
}

fn fixture_core(snapshot: &CatalogSnapshot, theme: &pf_theme::Theme, reduced: bool) -> ShellCore {
    ShellCore::boot_with_art(snapshot, theme, reduced, fixture_art)
}

fn catalog_art_paths(snapshot: &CatalogSnapshot) -> VecDeque<(String, PathBuf)> {
    snapshot
        .items
        .iter()
        .filter_map(|item| {
            let reference = item.presentation.icon_reference.clone()?;
            let manifest_dir = item
                .variants
                .first()?
                .launch_target
                .descriptor_path
                .parent()?
                .to_owned();
            Some((reference, manifest_dir))
        })
        .collect()
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

fn catalog_core(snapshot: &CatalogSnapshot, theme: &pf_theme::Theme, reduced: bool) -> ShellCore {
    let mut paths = catalog_art_paths(snapshot);
    ShellCore::boot_with_art(snapshot, theme, reduced, move |reference| {
        let index = paths
            .iter()
            .position(|(candidate, _)| candidate == reference)?;
        let (_, manifest_dir) = paths.remove(index)?;
        read_catalog_art(&manifest_dir, reference)
    })
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
    let catalog = (!fixture_mode)
        .then(|| installed_app_provider(&catalog_root, state_dir.join("favorites.json")));
    let snapshot: CatalogSnapshot = if let Some(provider) = &catalog {
        catalog_snapshot(provider, &catalog_root)?
    } else {
        serde_json::from_str(include_str!("../fixtures/catalog.json")).map_err(|e| e.to_string())?
    };
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
            catalog.as_ref().expect("fbdev catalog"),
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
            catalog.as_ref().expect("wayland catalog"),
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
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        emit(&mut host, &mut core, &footer, out, "settings")?;
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        emit(&mut host, &mut core, &footer, out, "controls")?;
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        emit(&mut host, &mut core, &footer, out, "network")?;
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
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
    core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
    core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
    core.action(&ShellAction::Activate);
    emit(host, &mut core, footer, out, "library")?;
    core.action(&ShellAction::Custom("Search".into()));
    core.set_search_query("ridgeline");
    emit(host, &mut core, footer, out, "search")?;
    core.action(&ShellAction::Activate);
    emit(host, &mut core, footer, out, "details")?;

    let mut chooser_snapshot = snapshot.clone();
    let item = chooser_snapshot
        .items
        .iter_mut()
        .find(|item| item.id == "glass-harbor")
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
    second.launch_target.app_id = "glass-harbor-handheld".into();
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
    match (&marker, phase) {
        (Some(path), ObservableAuthorityPhase::Starting) => {
            observe_desktop_sim_running(socket)?;
            println!("SUPERVISOR running marker={}", path.display());
        }
        (Some(path), ObservableAuthorityPhase::Running) => {
            println!("SUPERVISOR reconciled running marker={}", path.display());
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
    let marker =
        reconcile_desktop_sim_startup(socket, phase, || desktop_sim_marker(authority_state))?;
    let mut active_marker = marker;
    loop {
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

    // The desktop preset's title is deliberately only a marker-backed stub. Script its clean
    // exit, then feed the same supervisor observations used by the real authority lifecycle.
    fs::remove_file(&marker).map_err(|error| format!("remove stub marker: {error}"))?;
    observe_desktop_sim_return(socket, true)?;
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
                    core.session_backend_unavailable();
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
                core.session_backend_unavailable();
                break;
            }
            Err(error) => return Err(format!("session: {error:?}")),
        }
    }
    Ok(())
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
            "firstRunComplete" | "safeReturnBinding"
        ) {
            if change.authority != ChangeAuthority("user".into()) {
                return Ok(PreferenceChangeResult::Unauthorized);
            }
            let mut state = self.launcher_state()?;
            let value = match change.value {
                PreferenceValue::Bool(value) => serde_json::Value::Bool(value),
                PreferenceValue::Text(value) => serde_json::Value::String(value),
                PreferenceValue::Integer(value) => serde_json::Value::Number(value.into()),
            };
            state.insert(change.key.0, value);
            self.write_launcher_state(&state)?;
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
    present(host, core, prompt)?;
    let frame = host.frame().ok_or("frame missing")?;
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
    fn render_notes(&self) -> Option<&[RenderNote]>;
    fn raster_frame(&self) -> Option<&RasterFrame>;
}

impl RenderedFrameHost for OffscreenHost {
    fn render_notes(&self) -> Option<&[RenderNote]> {
        self.frame().map(|frame| frame.notes.as_slice())
    }

    fn raster_frame(&self) -> Option<&RasterFrame> {
        self.frame()
    }
}

impl RenderedFrameHost for FbdevHost {
    fn render_notes(&self) -> Option<&[RenderNote]> {
        self.frame().map(|frame| frame.notes.as_slice())
    }

    fn raster_frame(&self) -> Option<&RasterFrame> {
        self.frame()
    }
}

#[cfg(feature = "wayland")]
impl RenderedFrameHost for WaylandHost {
    fn render_notes(&self) -> Option<&[RenderNote]> {
        None
    }

    fn raster_frame(&self) -> Option<&RasterFrame> {
        None
    }
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
    host.set_palette(if core.high_contrast() {
        Palette::high_contrast()
    } else {
        Palette::standard()
    });
    host.present(scene).map_err(|e| e.to_string())?;
    let rejected = host
        .render_notes()
        .is_some_and(|notes| core.reject_art_sources(failed_source_ids(notes)));
    if rejected {
        host.present(
            core.scene(host.metrics(), prompt)
                .as_ref()
                .ok_or("shell has no fallback frame")?,
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
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
        let adopted = reconcile_desktop_sim_startup(&socket, phase, || {
            Ok(samples
                .next()
                .expect("startup sampled marker too many times"))
        })
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
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
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
        assert!(network_scene.contains("Scan unavailable · BackendUnavailable"));
        assert!(network_scene.contains("disabled: true"));
        assert_eq!(core.action(&ShellAction::Activate), None);

        core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
        let system_scene = format!("{:?}", core.scene(metrics, "").unwrap());
        assert!(
            system_scene.contains("Time status unavailable · BackendUnavailable"),
            "{system_scene}"
        );
        assert!(system_scene.contains("disabled: true"));
        assert_eq!(core.action(&ShellAction::Activate), None);
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
    fn variant_pin_cas_failure_changes_redraw_key_for_toast() {
        let mut snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        let item = snapshot
            .items
            .iter_mut()
            .find(|item| item.id == "glass-harbor")
            .unwrap();
        let mut second = item
            .variants
            .iter()
            .find(|variant| matches!(variant.availability, pf_catalog::Availability::Ready))
            .unwrap()
            .clone();
        second.id = "handheld".into();
        item.variants.push(second);
        let catalog = AlwaysConflictingFavorites {
            snapshot: snapshot.clone(),
        };
        let mut core = fixture_core(&snapshot, &pf_theme::flagship(), false);
        core.authority_snapshot(false);
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Move(pf_scene::AxisMove::Down));
        core.action(&ShellAction::Activate);
        let Effect::SetPinnedVariant {
            item_id,
            variant_id,
        } = core
            .action(&ShellAction::Custom("Favorite".into()))
            .expect("chooser exposes the mapped default affordance")
        else {
            panic!("chooser action must emit a pin effect");
        };
        let before_failure = redraw_state(&core);
        let status = commit_pinned_variant(&catalog, &item_id, variant_id.as_deref()).unwrap_err();
        core.pinned_variant_failed(status);
        assert_ne!(before_failure, redraw_state(&core));
        assert_eq!(
            core.session_status(),
            Some("Default version changed elsewhere; try again")
        );
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
        assert_ne!(host.frame().unwrap().rgba, last_frame);
    }

    #[test]
    fn corrupt_fixture_render_note_replaces_image_with_plate_on_redraw() {
        let mut snapshot: CatalogSnapshot =
            serde_json::from_str(include_str!("../fixtures/catalog.json")).unwrap();
        snapshot.items[0].presentation.icon_reference = Some("art/corrupt.png".into());
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
        assert_eq!(&standard.rgba[..3], &[13, 17, 23]);

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
