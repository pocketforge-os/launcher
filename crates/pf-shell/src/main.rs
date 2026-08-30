use pf_catalog::{CatalogSnapshot, InstalledAppProvider};
use pf_framehost::{FbdevHost, OffscreenHost};
use pf_input_map::{DeviceContract, EffectiveMap, JsonRemapStore, MemoryStore};
use pf_ports::{
    ActionEvent, ActionPoll, ActionSource, ChangeAuthority, Deadline, EffectivePreference,
    FakeNetworkPort, FakePowerPort, FakePreferencePort, FakeTimePort, FakeTransferPort, FrameHost,
    IdlePolicy, LaunchResult, MonotonicTime, NetworkPort, NetworkState, NtpState,
    ObservedSessionState, PowerAction, PowerCapability, PowerError, PowerPort, PowerRequestResult,
    PreferenceChange, PreferenceChangeResult, PreferenceError, PreferenceKey, PreferencePoll,
    PreferencePort, PreferenceValue, SessionError, SessionEvent, SessionPoll, SessionPort,
    ShellAction, Support, TerminalReceipt, TimeCapabilities, TimePort, TimeState, TransferPort,
    TransferService, TransferServiceState, WifiNetwork, WifiSecurity,
};
use pf_prefs::PrefsStore;
use pf_prefs_port::PrefsPreferencePort;
use pf_render::{RasterFrame, RenderNote};
use pf_scene::{Insets, Orientation, SurfaceMetrics};
use pf_session_authority::{EndPrecision, EndStamp, HistoryEntry};
use pf_session_client::{SessionClient, SocketTransport};
use pf_shell::{
    EvdevActionSource, FavoriteCatalog, GamepadRemap, commit_favorite, commit_pinned_variant,
    control_bindings, favorite_footer_prompt, footer_prompt, safe_return_options,
};
use pf_shell_core::{Effect, ShellCore};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    collections::VecDeque,
    env, fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

const DEFAULT_SESSION_SOCKET: &str = "/run/pocketforge/session-authority.sock";

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
            EffectiveMap::load(contract, &MemoryStore::default())
                .map_err(|fallback| format!("shipped input map: {fallback:?}"))
        }
    }
}

#[allow(clippy::too_many_lines)]
fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let fixture_mode = args
        .iter()
        .any(|a| matches!(a.as_str(), "--sim-frame" | "--settings-evidence"))
        || !args.iter().any(|a| a == "--fbdev");
    let state_dir = PathBuf::from(value(&args, "--state-dir").unwrap_or("./state"));
    let catalog = (!fixture_mode).then(|| {
        InstalledAppProvider::new(
            value(&args, "--catalog-root").unwrap_or("/opt/pocketforge/apps"),
            state_dir.join("favorites.json"),
            "native",
            "aarch64",
        )
    });
    let snapshot: CatalogSnapshot = if let Some(provider) = &catalog {
        provider
            .snapshot()
            .map_err(|error| format!("catalog: {error:?}"))?
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
    let mut core = fixture_core(&snapshot, &theme, reduced);
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
    let (mut network, mut time, mut transfer) = fixture_device_ports();
    core.load_network(&mut network);
    core.load_system(&time, &transfer);
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
        return run_fbdev(
            &mut host,
            &mut actions,
            &mut core,
            footer,
            preferences,
            power,
            glyphs,
            catalog.as_ref().expect("fbdev catalog"),
            Path::new(session_socket),
            &mut network,
            &mut time,
            &mut transfer,
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

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_fbdev(
    host: &mut FbdevHost,
    actions: &mut EvdevActionSource,
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
    present(host, core, &activate)?;
    let mut remap = GamepadRemap::with_store(map, remap_store);
    loop {
        let before = redraw_state(core);
        let poll = actions
            .next_action(deadline)
            .map_err(|e| format!("input: {e:?}"))?;
        drive_socket_session(core, &mut session)?;
        let ActionPoll::Event(ActionEvent::Action(action)) = poll else {
            if matches!(poll, ActionPoll::Closed) {
                return Ok(());
            }
            if before != redraw_state(core) {
                present(host, core, &activate)?;
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
                    present(host, core, &activate)?;
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
                    .frame()
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
            present(host, core, &activate)?;
        }
    }
}

trait ScreenshotWriter {
    fn write_png(&self, path: &Path, frame: &RasterFrame) -> Result<(), String>;
}

struct FsScreenshotWriter;

impl ScreenshotWriter for FsScreenshotWriter {
    fn write_png(&self, path: &Path, frame: &RasterFrame) -> Result<(), String> {
        let file = fs::File::create(path).map_err(|error| error.to_string())?;
        write_png(file, frame)
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
}

impl RenderedFrameHost for OffscreenHost {
    fn render_notes(&self) -> Option<&[RenderNote]> {
        self.frame().map(|frame| frame.notes.as_slice())
    }
}

impl RenderedFrameHost for FbdevHost {
    fn render_notes(&self) -> Option<&[RenderNote]> {
        self.frame().map(|frame| frame.notes.as_slice())
    }
}

fn present(
    host: &mut impl RenderedFrameHost,
    core: &mut ShellCore,
    prompt: &str,
) -> Result<(), String> {
    host.present(
        core.scene(host.metrics(), prompt)
            .as_ref()
            .ok_or("shell has no frame")?,
    )
    .map_err(|e| e.to_string())?;
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
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        out
    })
}

#[cfg(test)]
mod durable_tests {
    use super::*;

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
    fn corrupt_remap_file_falls_back_to_shipped_map() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("remaps.json");
        fs::write(&path, b"not json").unwrap();
        let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json")).unwrap();

        let map = load_durable_map_or_shipped(contract, &path).unwrap();

        assert!(
            control_bindings(&map)
                .iter()
                .any(|binding| { binding.action == "Activate" && binding.binding == "A" })
        );
    }
}
