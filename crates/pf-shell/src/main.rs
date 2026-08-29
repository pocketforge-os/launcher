use pf_catalog::CatalogSnapshot;
use pf_framehost::{FbdevHost, OffscreenHost};
use pf_input_map::{DeviceContract, EffectiveMap, MemoryStore};
use pf_ports::{
    ActionEvent, ActionPoll, ActionSource, Deadline, FrameHost, LaunchResult, MonotonicTime,
    ObservedSessionState, SessionError, SessionEvent, SessionPoll, SessionPort, ShellAction,
    TerminalReceipt,
};
use pf_scene::{Insets, Orientation, SurfaceMetrics};
use pf_shell::{EvdevActionSource, prompt};
use pf_shell_core::{Effect, ShellCore};
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    collections::VecDeque,
    env, fs,
    io::{BufWriter, Write},
    path::Path,
};

fn main() -> Result<(), String> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let snapshot: CatalogSnapshot = serde_json::from_str(include_str!("../fixtures/catalog.json"))
        .map_err(|e| e.to_string())?;
    let theme = pf_theme::flagship();
    let reduced = env::var_os("PF_REDUCE_MOTION").is_some();
    let contract = DeviceContract::parse_json(include_str!("../fixtures/device.json"))
        .map_err(|e| format!("{e:?}"))?;
    let glyphs =
        EffectiveMap::load(contract, &MemoryStore::default()).map_err(|e| format!("{e:?}"))?;
    let activate = prompt(&glyphs, &ShellAction::Activate);
    let mut core = ShellCore::boot(&snapshot, &theme, reduced);
    if args.iter().any(|a| a == "--sim-frame") {
        let path = value(&args, "--device").map_or_else(
            || env::var("PF_FB0").unwrap_or_else(|_| "/dev/fb0".into()),
            str::to_owned,
        );
        return emit_sim_frame(&core, &activate, Path::new(&path));
    }
    if args.iter().any(|a| a == "--fbdev") {
        let framebuffer = value(&args, "--device").unwrap_or("/dev/fb0");
        let input = value(&args, "--input").unwrap_or("/dev/input/event0");
        let mut host = FbdevHost::open(framebuffer).map_err(|e| e.to_string())?;
        let (mut actions, _) =
            EvdevActionSource::open(input, include_str!("../fixtures/device.json"))
                .map_err(|e| format!("input adapter: {e:?}"))?;
        host.present(&core.scene(host.metrics(), &activate))
            .map_err(|e| e.to_string())?;
        return run_fbdev(&mut host, &mut actions, &mut core, &activate);
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
    emit(&mut host, &core, &activate, out, "boot-home")?;
    core.action(&ShellAction::Move(pf_scene::AxisMove::Right));
    emit(&mut host, &core, &activate, out, "focus-moved")?;
    let effect = core
        .action(&ShellAction::Activate)
        .ok_or("fixture must launch")?;
    emit(&mut host, &core, &activate, out, "launch-dimmed")?;
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
    emit(&mut host, &core, &activate, out, "returned")?;
    Ok(())
}

fn emit_sim_frame(core: &ShellCore, prompt: &str, path: &Path) -> Result<(), String> {
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
    host.present(&core.scene(metrics, prompt))
        .map_err(|e| e.to_string())?;
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

fn run_fbdev(
    host: &mut FbdevHost,
    actions: &mut dyn ActionSource,
    core: &mut ShellCore,
    activate: &str,
) -> Result<(), String> {
    let deadline = Deadline(MonotonicTime::ZERO);
    let mut session = InteractiveSession::default();
    loop {
        let poll = actions
            .next_action(deadline)
            .map_err(|e| format!("input: {e:?}"))?;
        let ActionPoll::Event(ActionEvent::Action(action)) = poll else {
            if matches!(poll, ActionPoll::Closed) {
                return Ok(());
            }
            continue;
        };
        let before = (core.presentation(), core.focus());
        match core.action(&action) {
            Some(Effect::SafeReturn) => {
                session.safe_return();
                core.drive_session(&mut session)
                    .map_err(|e| format!("{e:?}"))?;
            }
            Some(Effect::Launch(request)) => {
                let result = session.launch(request).map_err(|e| format!("{e:?}"))?;
                core.launch_result(&result);
                host.present(&core.scene(host.metrics(), activate))
                    .map_err(|e| e.to_string())?;
                core.drive_session(&mut session)
                    .map_err(|e| format!("{e:?}"))?;
            }
            None => {}
        }
        if before != (core.presentation(), core.focus()) {
            // Rasterizer damage tracking makes unchanged parts of the retained
            // scene a no-op at the fbdev boundary.
            host.present(&core.scene(host.metrics(), activate))
                .map_err(|e| e.to_string())?;
        }
    }
}

#[derive(Default)]
struct InteractiveSession {
    active: Option<String>,
    pending: VecDeque<SessionEvent>,
    history: Vec<SessionEvent>,
}

impl InteractiveSession {
    fn safe_return(&mut self) {
        let Some(session_id) = self.active.take() else {
            return;
        };
        self.pending.extend([
            SessionEvent::Observed(ObservedSessionState::ObservationComplete),
            SessionEvent::Terminal(TerminalReceipt::Returned { session_id }),
        ]);
    }
}

impl SessionPort for InteractiveSession {
    fn launch(&mut self, request: pf_ports::LaunchRequest) -> Result<LaunchResult, SessionError> {
        if self.active.is_some() {
            return Ok(LaunchResult::RejectedBusy);
        }
        let session_id = format!("launcher-{}", request.item_id);
        self.active = Some(session_id.clone());
        self.pending
            .push_back(SessionEvent::Observed(ObservedSessionState::Running));
        Ok(LaunchResult::Accepted { session_id })
    }

    fn next_event(&mut self, _deadline: Deadline) -> Result<SessionPoll, SessionError> {
        let Some(event) = self.pending.pop_front() else {
            return Ok(SessionPoll::Idle);
        };
        self.history.push(event.clone());
        Ok(SessionPoll::Event(event))
    }

    fn history(&self) -> &[SessionEvent] {
        &self.history
    }
}

fn emit(
    host: &mut OffscreenHost,
    core: &ShellCore,
    prompt: &str,
    out: &Path,
    name: &str,
) -> Result<(), String> {
    host.present(&core.scene(host.metrics(), prompt))
        .map_err(|e| e.to_string())?;
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

fn value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2).find(|w| w[0] == key).map(|w| w[1].as_str())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        write!(out, "{byte:02x}").expect("writing to a String cannot fail");
        out
    })
}
