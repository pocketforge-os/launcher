//! Asynchronous physical-key notifications on the existing authenticated power socket.
//! A poll never consumes a request. Only the UI's successful presentation emits an ACK.
use serde_json::{Value, json};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const INTERVAL: Duration = Duration::from_millis(250);

/// Do not consume a request until the shell owns a ready frame and presentation succeeds.
pub fn present_pending(
    pending: &mut Option<String>,
    ready: bool,
    mut present: impl FnMut() -> Result<bool, String>,
    acknowledge: impl FnOnce(String),
) -> Result<bool, String> {
    if !ready || pending.is_none() {
        return Ok(false);
    }
    if present()? {
        if let Some(id) = pending.take() {
            acknowledge(id);
        }
    }
    Ok(true)
}

pub struct Client {
    events: Receiver<String>,
    ack: Option<Sender<String>>,
    worker: Option<JoinHandle<()>>,
}

impl Client {
    pub fn spawn(socket: PathBuf) -> std::io::Result<Self> {
        let (events_tx, events) = mpsc::sync_channel(1);
        let (ack, ack_rx) = mpsc::channel();
        let worker = thread::Builder::new()
            .name("pf-power-menu".into())
            .spawn(move || {
                let mut state = Delivery::default();
                loop {
                    match ack_rx.recv_timeout(INTERVAL) {
                        Ok(id) => state.ack = Some(id),
                        Err(RecvTimeoutError::Timeout) => {}
                        Err(RecvTimeoutError::Disconnected) => break,
                    }
                    if !state.step(|request| call(&socket, request), &events_tx) {
                        break;
                    }
                }
            })?;
        Ok(Self {
            events,
            ack: Some(ack),
            worker: Some(worker),
        })
    }

    pub fn pending(&self) -> Option<String> {
        self.events.try_iter().last()
    }

    pub fn presented(&self, id: String) {
        if let Some(ack) = &self.ack {
            let _ = ack.send(id);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.ack.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Default)]
struct Delivery {
    delivered: Option<String>,
    ack: Option<String>,
}

impl Delivery {
    fn step(
        &mut self,
        mut call: impl FnMut(&Value) -> Result<Value, ()>,
        events: &SyncSender<String>,
    ) -> bool {
        if let Some(id) = &self.ack {
            if let Ok(reply) = call(&json!({"method":"menu_ack", "request_id":id})) {
                // false means a newer physical press superseded this request. Neither
                // outcome grants permission for any power command.
                if reply.get("result").and_then(Value::as_str) == Some("menu_ack")
                    && reply.get("acknowledged").and_then(Value::as_bool).is_some()
                {
                    self.ack = None;
                }
            }
        }
        let Ok(reply) = call(&json!({"method":"menu_poll"})) else {
            return true;
        };
        if reply.get("result").and_then(Value::as_str) != Some("menu") {
            return true;
        }
        let Some(id) = reply.get("request_id").and_then(Value::as_str) else {
            if reply.get("request_id").is_some_and(Value::is_null) {
                self.delivered = None;
            }
            return true;
        };
        if id.is_empty() || id.len() > 128 || self.delivered.as_deref() == Some(id) {
            return true;
        }
        match events.try_send(id.to_owned()) {
            Ok(()) => self.delivered = Some(id.to_owned()),
            Err(TrySendError::Full(_)) => {} // bounded queue; retry after the UI drains it
            Err(TrySendError::Disconnected(_)) => return false,
        }
        true
    }
}

fn call(socket: &Path, request: &Value) -> Result<Value, ()> {
    let mut stream = UnixStream::connect(socket).map_err(|_| ())?;
    stream.set_read_timeout(Some(INTERVAL)).map_err(|_| ())?;
    stream.set_write_timeout(Some(INTERVAL)).map_err(|_| ())?;
    let body = serde_json::to_vec(request).map_err(|_| ())?;
    pf_wire::write_frame(&mut stream, &body).map_err(|_| ())?;
    let reply = pf_wire::read_frame(&mut stream).map_err(|_| ())?;
    serde_json::from_slice(&reply).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn power_key_presentation_failure_and_foreground_app_keep_request() {
        let mut pending = Some("boot:1".into());
        assert!(
            !present_pending(
                &mut pending,
                false,
                || panic!("app must keep its frame"),
                |_| panic!("no ACK")
            )
            .unwrap()
        );
        assert!(
            present_pending(
                &mut pending,
                true,
                || Ok(false),
                |_| panic!("no frame, no ACK")
            )
            .unwrap()
        );
        assert!(
            present_pending(
                &mut pending,
                true,
                || Err("renderer failed".into()),
                |_| panic!("failed frame, no ACK")
            )
            .is_err()
        );
        assert_eq!(pending.as_deref(), Some("boot:1"));
        let mut acknowledged = None;
        assert!(
            present_pending(
                &mut pending,
                true,
                || Ok(true),
                |id| acknowledged = Some(id)
            )
            .unwrap()
        );
        assert_eq!(acknowledged.as_deref(), Some("boot:1"));
        assert!(pending.is_none());
    }

    #[test]
    fn power_key_full_queue_retries_newer_request_and_refused_ack_is_harmless() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut state = Delivery::default();
        state.step(|_| Ok(json!({"result":"menu","request_id":"boot:1"})), &tx);
        state.step(|_| Ok(json!({"result":"menu","request_id":"boot:2"})), &tx);
        assert_eq!(rx.try_recv().unwrap(), "boot:1");
        state.ack = Some("boot:1".into());
        state.step(
            |request| {
                if request["method"] == "menu_ack" {
                    Ok(json!({"result":"menu_ack", "acknowledged":false}))
                } else {
                    Ok(json!({"result":"menu","request_id":"boot:2"}))
                }
            },
            &tx,
        );
        assert_eq!(rx.try_recv().unwrap(), "boot:2");
        assert!(state.ack.is_none());
    }
    #[test]
    fn power_key_ack_follows_presentation_and_retries_without_reopening() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut state = Delivery::default();
        let menu = json!({"result":"menu", "request_id":"incarnation:1"});
        let mut calls = Vec::new();
        state.step(
            |request| {
                calls.push(request.clone());
                Ok(menu.clone())
            },
            &tx,
        );
        let id = rx.try_recv().unwrap();
        assert_eq!(calls, vec![json!({"method":"menu_poll"})]);
        state.step(|_| Ok(menu.clone()), &tx);
        assert!(rx.try_recv().is_err());
        // The UI has now successfully presented. A failed ACK is retried, but
        // does not repeatedly open the sheet or activate its current focus.
        state.ack = Some(id);
        state.step(
            |request| {
                if request["method"] == "menu_ack" {
                    Err(())
                } else {
                    Ok(menu.clone())
                }
            },
            &tx,
        );
        assert!(state.ack.is_some());
        assert!(rx.try_recv().is_err());
        state.step(
            |request| {
                if request["method"] == "menu_ack" {
                    Ok(json!({"result":"menu_ack", "acknowledged":true}))
                } else {
                    Ok(json!({"result":"menu", "request_id":null}))
                }
            },
            &tx,
        );
        assert!(state.ack.is_none());
        assert!(rx.try_recv().is_err());
        // A restarted shell has no local delivery state and rediscovers an
        // unacknowledged request; a restarted daemon uses a different ID.
        let mut restarted = Delivery::default();
        restarted.step(|_| Ok(menu.clone()), &tx);
        assert_eq!(rx.try_recv().unwrap(), "incarnation:1");
        restarted.step(|_| Ok(json!({"result":"menu","request_id":"new:1"})), &tx);
        assert_eq!(rx.try_recv().unwrap(), "new:1");
    }

    #[test]
    fn power_key_real_socket_uses_framed_menu_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("power.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let body = pf_wire::read_frame(&mut stream).unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"method":"menu_poll"})
            );
            pf_wire::write_frame(&mut stream, br#"{"result":"menu","request_id":"boot:1"}"#)
                .unwrap();
        });
        assert_eq!(
            call(&path, &json!({"method":"menu_poll"})).unwrap()["request_id"],
            "boot:1"
        );
        server.join().unwrap();
    }
}
