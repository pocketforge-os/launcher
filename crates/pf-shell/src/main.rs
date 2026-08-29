use pf_catalog::{CatalogSnapshot, InstalledAppProvider};
use pf_framehost::{FbdevHost, OffscreenHost};
use pf_input_map::{DeviceContract, EffectiveMap, MemoryStore};
use pf_ports::{
    ActionEvent, ActionPoll, ActionSource, ChangeAuthority, Deadline, EffectivePreference,
    FakePreferencePort, FrameHost, LaunchResult, MonotonicTime, ObservedSessionState,
    PreferenceChange, PreferenceChangeResult, PreferenceError, PreferenceKey, PreferencePoll,
    PreferencePort, PreferenceValue, SessionError, SessionEvent, SessionPoll, SessionPort,
    ShellAction, TerminalReceipt,
};
use pf_prefs::PrefsStore;
use pf_prefs_port::PrefsPreferencePort;
use pf_render::RenderNote;
use pf_scene::{Insets, Orientation, SurfaceMetrics};
use pf_session_client::{SessionClient, SocketTransport};
use pf_shell::{
    EvdevActionSource, FavoriteCatalog, GamepadRemap, commit_favorite, favorite_footer_prompt,
    footer_prompt, safe_return_options,
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
    let glyphs =
        EffectiveMap::load(contract, &MemoryStore::default()).map_err(|e| format!("{e:?}"))?;
    let mut footer = footer_prompt(&glyphs);
    if let Some(hint) = favorite_footer_prompt(&glyphs, false) {
        if let Some(glyph) = hint.strip_suffix("  Favorite") {
            footer.push('\u{1f}');
            footer.push_str(glyph);
        }
    }
    let mut core = fixture_core(&snapshot, &theme, reduced);
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
        let (mut actions, _) =
            EvdevActionSource::open(input, include_str!("../fixtures/device.json"))
                .map_err(|e| format!("input adapter: {e:?}"))?;
        let session_socket = value(&args, "--session-socket").unwrap_or(DEFAULT_SESSION_SOCKET);
        return run_fbdev(
            &mut host,
            &mut actions,
            &mut core,
            &footer,
            preferences,
            glyphs,
            catalog.as_ref().expect("fbdev catalog"),
            Path::new(session_socket),
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

#[allow(clippy::too_many_arguments)]
fn run_fbdev(
    host: &mut FbdevHost,
    actions: &mut dyn ActionSource,
    core: &mut ShellCore,
    activate: &str,
    preferences: &mut dyn PreferencePort,
    map: EffectiveMap,
    catalog: &dyn FavoriteCatalog,
    session_socket: &Path,
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
    present(host, core, activate)?;
    let mut remap = GamepadRemap::new(map);
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
                present(host, core, activate)?;
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
                    present(host, core, activate)?;
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
            Some(Effect::BeginRemap) => {
                remap
                    .preview("global", "Activate", pf_input_map::Binding::single("north"))
                    .map_err(|e| format!("remap preview: {e:?}"))?;
            }
            Some(Effect::ConfirmRemap) => {
                remap
                    .gamepad_action(&ShellAction::Activate)
                    .map_err(|e| format!("remap confirm: {e:?}"))?;
            }
            Some(Effect::RollbackRemap) => {
                remap
                    .gamepad_action(&ShellAction::Back)
                    .map_err(|e| format!("remap rollback: {e:?}"))?;
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
            None => {}
        }
        if before != redraw_state(core) {
            // Rasterizer damage tracking makes unchanged parts of the retained
            // scene a no-op at the fbdev boundary.
            present(host, core, activate)?;
        }
    }
}

fn redraw_state(core: &ShellCore) -> (pf_shell_core::Presentation, usize, bool, Option<String>) {
    (
        core.presentation().clone(),
        core.focus(),
        core.authority_unavailable(),
        core.session_status().map(str::to_owned),
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

    struct UnavailableSession;

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
}
