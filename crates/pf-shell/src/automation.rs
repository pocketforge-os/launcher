use pf_scene::{Node, NodeAction, NodeContent, Scene, SurfaceMetrics};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, PartialOrd, Ord)]
pub struct FrameCounter(u64);

impl FrameCounter {
    pub fn increment(&mut self) {
        self.0 += 1;
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Ping,
    Scene,
    Capture { path: PathBuf },
    Text { value: String },
    WaitIdle { quiet: Duration, timeout: Duration },
}

struct Client {
    stream: UnixStream,
    input: Vec<u8>,
    output: VecDeque<Vec<u8>>,
    waiting: Option<Wait>,
    closed: bool,
}

struct Wait {
    started: Instant,
    quiet: Duration,
    timeout: Duration,
}

pub struct AutomationServer {
    listener: UnixListener,
    clients: Vec<Client>,
    socket_path: PathBuf,
    last_action: Instant,
    now: Box<dyn Fn() -> Instant>,
}

pub struct Snapshot<'a> {
    pub frames: FrameCounter,
    pub revision: u64,
    pub presented_revision: u64,
    pub input_pending: bool,
    pub route: &'a str,
    pub input_source: &'a str,
    pub metrics: SurfaceMetrics,
    pub text_scale: u16,
    pub high_contrast: bool,
    pub search_query: &'a str,
    pub search_result_ids: &'a [&'a str],
    pub scene: Option<&'a Scene>,
}

pub struct Request {
    client: usize,
    pub command: Command,
}

impl AutomationServer {
    pub fn bind(path: &Path) -> Result<Self, String> {
        Self::bind_with_clock(path, Instant::now)
    }

    fn bind_with_clock(path: &Path, now: impl Fn() -> Instant + 'static) -> Result<Self, String> {
        match fs::symlink_metadata(path) {
            Ok(metadata) if !metadata.file_type().is_socket() => {
                return Err(format!(
                    "automation socket: {} exists and is not a socket",
                    path.display()
                ));
            }
            Ok(_) => match UnixStream::connect(path) {
                Ok(_) => {
                    return Err(format!(
                        "automation socket: {} is already in use",
                        path.display()
                    ));
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionRefused | ErrorKind::NotFound
                    ) =>
                {
                    match fs::remove_file(path) {
                        Ok(()) => {}
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => return Err(format!("automation socket: {error}")),
                    }
                }
                Err(error) => return Err(format!("automation socket: {error}")),
            },
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(format!("automation socket: {error}")),
        }
        let listener = UnixListener::bind(path).map_err(|e| format!("automation socket: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let now = Box::new(now);
        let last_action = now();
        Ok(Self {
            listener,
            clients: Vec::new(),
            socket_path: path.to_path_buf(),
            last_action,
            now,
        })
    }

    pub fn note_action(&mut self) {
        self.last_action = (self.now)();
    }

    pub fn poll(&mut self, snapshot: &Snapshot<'_>) -> Result<Vec<Request>, String> {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(true).map_err(|e| e.to_string())?;
                    self.clients.push(Client {
                        stream,
                        input: Vec::new(),
                        output: VecDeque::new(),
                        waiting: None,
                        closed: false,
                    });
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(format!("automation accept: {e}")),
            }
        }
        let now = (self.now)();
        let mut requests = Vec::new();
        for (index, client) in self.clients.iter_mut().enumerate() {
            let mut bytes = [0_u8; 8192];
            loop {
                match client.stream.read(&mut bytes) {
                    Ok(0) => {
                        client.closed = true;
                        break;
                    }
                    Ok(n) => client.input.extend_from_slice(&bytes[..n]),
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) => return Err(format!("automation read: {e}")),
                }
            }
            while client.waiting.is_none() {
                let Some(end) = client.input.iter().position(|b| *b == b'\n') else {
                    break;
                };
                let line = client.input.drain(..=end).collect::<Vec<_>>();
                match parse(&line[..line.len() - 1]) {
                    Ok(command @ Command::WaitIdle { quiet, timeout }) => {
                        client.waiting = Some(Wait {
                            started: now,
                            quiet,
                            timeout,
                        });
                        requests.push(Request {
                            client: index,
                            command,
                        });
                    }
                    Ok(command) => requests.push(Request {
                        client: index,
                        command,
                    }),
                    Err(error) => client
                        .output
                        .push_back(response(false, snapshot, Some(&error))),
                }
            }
            if let Some(wait) = &client.waiting {
                let idle = now.duration_since(self.last_action) >= wait.quiet
                    && !snapshot.input_pending
                    && snapshot.presented_revision >= snapshot.revision;
                if idle || now.duration_since(wait.started) >= wait.timeout {
                    let error = (!idle).then_some("timeout");
                    client.output.push_back(response(idle, snapshot, error));
                    client.waiting = None;
                }
            }
            flush(client)?;
        }
        if requests.is_empty() {
            self.clients
                .retain(|client| !(client.closed && client.output.is_empty()));
        }
        Ok(requests)
    }

    pub fn reply(
        &mut self,
        request: &Request,
        value: &Value,
        snapshot: &Snapshot<'_>,
    ) -> Result<(), String> {
        if matches!(request.command, Command::WaitIdle { .. }) {
            return Ok(());
        }
        let mut object = value.as_object().cloned().unwrap_or_default();
        object.insert("frames".into(), json!(snapshot.frames.get()));
        object.insert("revision".into(), json!(snapshot.revision));
        let mut line = serde_json::to_vec(&object).map_err(|e| e.to_string())?;
        line.push(b'\n');
        if let Some(client) = self.clients.get_mut(request.client) {
            client.output.push_back(line);
            flush(client)?;
        }
        Ok(())
    }
}

impl Drop for AutomationServer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
    }
}

fn flush(client: &mut Client) -> Result<(), String> {
    while let Some(front) = client.output.front_mut() {
        match client.stream.write(front) {
            Ok(0) => break,
            Ok(n) => {
                front.drain(..n);
                if front.is_empty() {
                    client.output.pop_front();
                }
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) if matches!(e.kind(), ErrorKind::BrokenPipe | ErrorKind::ConnectionReset) => {
                client.closed = true;
                client.output.clear();
                break;
            }
            Err(e) => return Err(format!("automation write: {e}")),
        }
    }
    Ok(())
}

fn response(ok: bool, snapshot: &Snapshot<'_>, error: Option<&str>) -> Vec<u8> {
    let mut value =
        json!({"ok": ok, "frames": snapshot.frames.get(), "revision": snapshot.revision});
    if let Some(error) = error {
        value["error"] = json!(error);
    }
    let mut bytes = serde_json::to_vec(&value).expect("JSON response");
    bytes.push(b'\n');
    bytes
}

pub fn parse(line: &[u8]) -> Result<Command, String> {
    let value: Value = serde_json::from_slice(line).map_err(|_| "invalid_json".to_owned())?;
    let object = value.as_object().ok_or_else(|| "invalid_json".to_owned())?;
    let op = object
        .get("op")
        .and_then(Value::as_str)
        .ok_or_else(|| "missing_field:op".to_owned())?;
    let string = |name: &str| {
        object
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("missing_field:{name}"))
    };
    let millis = |name: &str, default| {
        object.get(name).map_or(Ok(default), |v| {
            v.as_u64().ok_or_else(|| format!("missing_field:{name}"))
        })
    };
    match op {
        "ping" => Ok(Command::Ping),
        "scene" => Ok(Command::Scene),
        "capture" => Ok(Command::Capture {
            path: PathBuf::from(string("path")?),
        }),
        "text" => Ok(Command::Text {
            value: string("value")?,
        }),
        "wait_idle" => Ok(Command::WaitIdle {
            quiet: Duration::from_millis(millis("quiet_ms", 150)?),
            timeout: Duration::from_millis(millis("timeout_ms", 5000)?),
        }),
        _ => Err("unknown_op".into()),
    }
}

pub fn ping(snapshot: &Snapshot<'_>) -> Value {
    json!({"ok": true, "route": snapshot.route, "input_source": snapshot.input_source})
}

pub fn scene(snapshot: &Snapshot<'_>) -> Value {
    let scene = snapshot.scene;
    json!({"ok": true,
        "metrics": {"width": snapshot.metrics.logical_width, "height": snapshot.metrics.logical_height, "scale": snapshot.metrics.scale},
        "text_scale": format!("{}%", snapshot.text_scale), "high_contrast": snapshot.high_contrast,
        "search_query": snapshot.search_query, "search_result_ids": snapshot.search_result_ids,
        "default_focus": scene.map(|s| s.default_focus().as_str()), "focused": scene.and_then(Scene::focused).map(pf_scene::NodeId::as_str),
        "scene": scene.map(|s| node(s.root()))})
}

fn debug<T: std::fmt::Debug>(value: &T) -> String {
    format!("{value:?}").to_ascii_lowercase().replace('_', "-")
}
fn node(value: &Node) -> Value {
    let action = value.action.as_ref().map(|a| match a {
        NodeAction::Activate => "activate".into(),
        NodeAction::Back => "back".into(),
        NodeAction::SetValue(v) => format!("set-value:{v}"),
        NodeAction::Custom(v) => v.clone(),
    });
    let content = match &value.content {
        NodeContent::Label => json!({"kind":"text","text":value.accessible_label}),
        NodeContent::Image { source, fit } => {
            json!({"kind":"art","source":source.id,"fit":debug(fit)})
        }
    };
    json!({"id":value.id.as_str(),"role":debug(&value.role),"accessible_label":value.accessible_label,"state":{
        "focused":value.state.focused,"pressed":value.state.pressed,"disabled":value.state.disabled,"selected":value.state.selected,"unavailable":value.state.unavailable,"destructive":value.state.destructive,"scrimmed":value.state.scrimmed,"checked":value.state.checked,"expanded":value.state.expanded},
        "bounds":{"x":value.bounds.x,"y":value.bounds.y,"width":value.bounds.width,"height":value.bounds.height,"min_width":value.bounds.min_width,"min_height":value.bounds.min_height,"max_width":value.bounds.max_width,"max_height":value.bounds.max_height},
        "layout":value.layout.as_ref().map(debug),"style_token":value.style_token,"action":action,"content":content,"type_role":debug(&value.type_role),"line_height":value.line_height,"text_align":debug(&value.text_align),"ink_token":value.ink_token,"fixed_paint_scale":value.fixed_paint_scale,"corner_radius":value.corner_radius,"border_token":value.border_token,"border_width":value.border_width,"elevation":debug(&value.elevation),"children":value.children.iter().map(node).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_scene::{Bounds, ImageFit, ImageSource, NodeId, Role};
    use std::io::{BufRead, BufReader};
    use std::sync::Arc;
    use std::sync::Mutex;
    #[test]
    fn parses_commands_and_errors() {
        assert_eq!(parse(br#"{"op":"ping"}"#), Ok(Command::Ping));
        assert_eq!(parse(br#"{"op":"scene"}"#), Ok(Command::Scene));
        assert_eq!(
            parse(br#"{"op":"capture","path":"/tmp/a.png"}"#),
            Ok(Command::Capture {
                path: PathBuf::from("/tmp/a.png")
            })
        );
        assert_eq!(
            parse(br#"{"op":"text","value":"e"}"#),
            Ok(Command::Text { value: "e".into() })
        );
        assert_eq!(parse(b"{"), Err("invalid_json".into()));
        assert_eq!(parse(br#"{"op":"wat"}"#), Err("unknown_op".into()));
        assert_eq!(
            parse(br#"{"op":"wait_idle"}"#),
            Ok(Command::WaitIdle {
                quiet: Duration::from_millis(150),
                timeout: Duration::from_secs(5)
            })
        );
        assert_eq!(
            parse(br#"{"op":"capture"}"#),
            Err("missing_field:path".into())
        );
    }

    #[test]
    fn scene_encoder_preserves_nested_text_art_and_focus() {
        let child = Node::new(
            NodeId::new("child").unwrap(),
            Role::Text,
            "Hello",
            Bounds::new(1.0, 2.0, 3.0, 4.0),
            "text",
        )
        .with_image(
            ImageSource::new("art:one", Arc::<[u8]>::from([1_u8, 2])),
            ImageFit::Contain,
        );
        let root = Node::new(
            NodeId::new("root").unwrap(),
            Role::Button,
            "Open",
            Bounds::new(0.0, 0.0, 10.0, 10.0),
            "button",
        )
        .with_action(NodeAction::Activate)
        .with_children(vec![child]);
        let scene_value = Scene::new(root, NodeId::new("root").unwrap()).unwrap();
        let ids = ["one"];
        let snapshot = Snapshot {
            frames: FrameCounter::default(),
            revision: 7,
            presented_revision: 7,
            input_pending: false,
            route: "home",
            input_source: "evdev",
            metrics: SurfaceMetrics {
                logical_width: 10.0,
                logical_height: 10.0,
                scale: 1.0,
                safe_insets: pf_scene::Insets::default(),
                orientation: pf_scene::Orientation::Landscape,
            },
            text_scale: 150,
            high_contrast: true,
            search_query: "o",
            search_result_ids: &ids,
            scene: Some(&scene_value),
        };
        let encoded = scene(&snapshot);
        assert_eq!(encoded["default_focus"], "root");
        assert_eq!(encoded["focused"], "root");
        assert_eq!(
            encoded["scene"]["children"][0]["content"]["source"],
            "art:one"
        );
        assert_eq!(encoded["text_scale"], "150%");
        assert_eq!(encoded["search_result_ids"], json!(["one"]));
    }

    fn empty_snapshot() -> Snapshot<'static> {
        Snapshot {
            frames: FrameCounter::default(),
            revision: 1,
            presented_revision: 1,
            input_pending: false,
            route: "home",
            input_source: "evdev",
            metrics: SurfaceMetrics {
                logical_width: 1.0,
                logical_height: 1.0,
                scale: 1.0,
                safe_insets: pf_scene::Insets::default(),
                orientation: pf_scene::Orientation::Landscape,
            },
            text_scale: 100,
            high_contrast: false,
            search_query: "",
            search_result_ids: &[],
            scene: None,
        }
    }

    #[test]
    fn bind_refuses_regular_file_without_modifying_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto.sock");
        fs::write(&path, b"keep me").unwrap();

        let error = AutomationServer::bind(&path).err().unwrap();

        assert_eq!(
            error,
            format!(
                "automation socket: {} exists and is not a socket",
                path.display()
            )
        );
        assert_eq!(fs::read(&path).unwrap(), b"keep me");
    }

    #[test]
    fn bind_reclaims_stale_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto.sock");
        let listener = UnixListener::bind(&path).unwrap();
        drop(listener);

        let _server = AutomationServer::bind(&path).unwrap();
    }

    #[test]
    fn bind_refuses_live_socket_without_unlinking_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auto.sock");
        let listener = UnixListener::bind(&path).unwrap();

        let error = AutomationServer::bind(&path).err().unwrap();

        assert_eq!(
            error,
            format!("automation socket: {} is already in use", path.display())
        );
        let (_stream, _) = listener.accept().unwrap();
    }

    #[test]
    fn wait_idle_uses_injected_clock_for_satisfaction_and_timeout() {
        for (advance, expected_ok) in [
            (Duration::from_millis(150), true),
            (Duration::from_millis(6), false),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("auto.sock");
            let clock = Arc::new(Mutex::new(Instant::now()));
            let read_clock = Arc::clone(&clock);
            let mut server =
                AutomationServer::bind_with_clock(&path, move || *read_clock.lock().unwrap())
                    .unwrap();
            server.note_action();
            let mut client = UnixStream::connect(&path).unwrap();
            let command = if expected_ok {
                b"{\"op\":\"wait_idle\"}\n".as_slice()
            } else {
                b"{\"op\":\"wait_idle\",\"quiet_ms\":100,\"timeout_ms\":5}\n".as_slice()
            };
            client.write_all(command).unwrap();
            server.poll(&empty_snapshot()).unwrap();
            *clock.lock().unwrap() += advance;
            server.poll(&empty_snapshot()).unwrap();
            let mut line = String::new();
            BufReader::new(client).read_line(&mut line).unwrap();
            let response: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(response["ok"], expected_ok);
            if !expected_ok {
                assert_eq!(response["error"], "timeout");
            }
        }
    }
}
