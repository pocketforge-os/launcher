//! Pure reducer and Quiet Console Home scene for the F09a vertical slice.
#![allow(
    clippy::cast_precision_loss,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::too_many_arguments,
    clippy::too_many_lines
)]

use pf_catalog::{Availability, CatalogSnapshot};
use pf_ports::{
    Deadline, LaunchRequest, LaunchResult, MonotonicTime, ObservedSessionState, SessionEvent,
    SessionPoll, SessionPort, ShellAction, TerminalReceipt,
};
use pf_scene::{AxisMove, Bounds, Node, NodeAction, NodeId, Role, Scene, SurfaceMetrics};
use pf_theme::Theme;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Route {
    Home,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Presentation {
    Home,
    LaunchDimmed,
    AppRunning,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Launch(LaunchRequest),
    SafeReturn,
}

pub struct ShellCore {
    route: Route,
    presentation: Presentation,
    ready: Vec<(String, String)>,
    focus: usize,
    restore_focus: usize,
    reduced_motion: bool,
    motion_ms: u32,
}

impl ShellCore {
    #[must_use]
    pub fn boot(snapshot: &CatalogSnapshot, theme: &Theme, reduced_motion: bool) -> Self {
        let ready = snapshot
            .items
            .iter()
            .filter_map(|item| {
                item.variants
                    .iter()
                    .find(|variant| matches!(variant.availability, Availability::Ready))
                    .map(|variant| (variant.launch_target.app_id.clone(), item.title.clone()))
            })
            .collect::<Vec<_>>();
        let motion_ms = theme
            .resolve_motion("launch", reduced_motion)
            .expect("flagship contains motion.launch")
            .duration_ms;
        Self {
            route: Route::Home,
            presentation: Presentation::Home,
            ready,
            focus: 0,
            restore_focus: 0,
            reduced_motion,
            motion_ms,
        }
    }

    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }
    #[must_use]
    pub const fn presentation(&self) -> Presentation {
        self.presentation
    }
    #[must_use]
    pub const fn focus(&self) -> usize {
        self.focus
    }
    #[must_use]
    pub const fn motion_duration_ms(&self) -> u32 {
        self.motion_ms
    }
    #[must_use]
    pub const fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    pub fn action(&mut self, action: &ShellAction) -> Option<Effect> {
        // Safe Return is a global, protected action. It must reach the session
        // authority while the shelf is visible, dimming, or owned by an app.
        if matches!(action, ShellAction::Custom(name) if name == "SafeReturn") {
            return Some(Effect::SafeReturn);
        }
        if self.presentation != Presentation::Home {
            return None;
        }
        match action {
            ShellAction::Move(AxisMove::Right | AxisMove::Down) => {
                if self.focus + 1 < self.ready.len() {
                    self.focus += 1;
                }
            }
            ShellAction::Move(AxisMove::Left | AxisMove::Up) | ShellAction::Back => {
                self.focus = self.focus.saturating_sub(1);
            }
            ShellAction::Activate => {
                let (id, _) = self.ready.get(self.focus)?;
                self.restore_focus = self.focus;
                self.presentation = Presentation::LaunchDimmed;
                return Some(Effect::Launch(LaunchRequest {
                    item_id: id.clone(),
                }));
            }
            ShellAction::Custom(_) => {}
        }
        None
    }

    pub fn launch_result(&mut self, result: &LaunchResult) {
        if matches!(result, LaunchResult::Accepted { .. }) {
            self.presentation = Presentation::AppRunning;
        } else {
            self.presentation = Presentation::Home;
        }
    }

    pub fn session_event(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Observed(ObservedSessionState::Running) => {
                self.presentation = Presentation::AppRunning;
            }
            SessionEvent::Terminal(TerminalReceipt::Returned { .. }) => {
                self.route = Route::Home;
                self.focus = self.restore_focus;
                self.presentation = Presentation::Home;
            }
            _ => {}
        }
    }

    pub fn drive_session(
        &mut self,
        port: &mut dyn SessionPort,
    ) -> Result<(), pf_ports::SessionError> {
        while let SessionPoll::Event(event) = port.next_event(Deadline(MonotonicTime::ZERO))? {
            self.session_event(&event);
        }
        Ok(())
    }

    #[must_use]
    pub fn scene(&self, metrics: SurfaceMetrics, footer_prompt: &str) -> Scene {
        let w = metrics.logical_width;
        let h = metrics.logical_height;
        let margin = if w >= 1024.0 { 48.0 } else { 32.0 };
        let dimmed = self.presentation != Presentation::Home;
        let focused_title = self
            .ready
            .get(self.focus)
            .map_or("Nothing ready", |(_, title)| title.as_str());
        let mut children = vec![
            node(
                "status-left-spacer",
                Role::Text,
                "",
                0.0,
                0.0,
                200.0,
                64.0,
                "--color-surface-canvas",
            ),
            node(
                "rooms",
                Role::Text,
                "L     Home     Library     Settings     R",
                w / 2.0 - 220.0,
                16.0,
                440.0,
                32.0,
                "--state-rest-text",
            ),
            node(
                "room-underline",
                Role::Text,
                "",
                w / 2.0 - 125.0,
                50.0,
                42.0,
                3.0,
                "--state-selected-accent",
            ),
            node(
                "system",
                Role::Text,
                "Wi-Fi   82%   9:41",
                w - 248.0,
                16.0,
                200.0,
                32.0,
                "--color-text-secondary",
            ),
            node(
                "hero-eyebrow",
                Role::Text,
                "RECENT · TONIGHT",
                margin,
                118.0,
                240.0,
                28.0,
                "--color-text-muted",
            ),
            node(
                "hero-title",
                Role::Heading,
                focused_title,
                margin,
                154.0,
                620.0,
                64.0,
                "--state-rest-text",
            ),
            node(
                "hero-status",
                Role::Text,
                if dimmed {
                    "● Starting · Game · Installed"
                } else {
                    "● Ready · Game · Installed"
                },
                margin,
                226.0,
                480.0,
                32.0,
                if dimmed {
                    "--color-text-muted"
                } else {
                    "--color-status-ready"
                },
            ),
            node(
                "ready-heading",
                Role::Heading,
                &format!("READY NOW · {}", self.ready.len()),
                margin,
                398.0,
                220.0,
                28.0,
                "--color-text-muted",
            ),
        ];
        let gap = 24.0;
        let card_w = 158.0_f32.min((w - margin * 2.0) / self.ready.len().max(1) as f32 - gap);
        for (index, (id, title)) in self.ready.iter().enumerate() {
            let x = margin + index as f32 * (card_w + gap);
            let mut card = node(
                &format!("ready-{id}"),
                Role::Button,
                "",
                x,
                430.0,
                card_w,
                210.0,
                if index == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            card.action = Some(NodeAction::Activate);
            card.state.focused = index == self.focus;
            card.state.disabled = dimmed;
            let monogram = title.chars().next().unwrap_or('·').to_string();
            let motif = match stable_plate(id) % 6 {
                0 => "╱  ╱  ╱\n  ╱  ╱",
                1 => "≈ ≈ ≈\n ≈ ≈ ≈",
                2 => "· · · ·\n · · ·",
                3 => "○   ◌\n  ◉",
                4 => "⌁ ⌁ ⌁\n ⌁ ⌁",
                _ => "\\ | /\n— ◉ —",
            };
            card.children = vec![
                node(
                    &format!("plate-motif-{id}"),
                    Role::Text,
                    motif,
                    x + 8.0,
                    438.0,
                    card_w - 16.0,
                    60.0,
                    plate_token(stable_plate(id)),
                ),
                node(
                    &format!("plate-mono-{id}"),
                    Role::Text,
                    &monogram,
                    x + 42.0,
                    510.0,
                    card_w - 84.0,
                    58.0,
                    plate_token(stable_plate(id)),
                ),
                node(
                    &format!("plate-kind-{id}"),
                    Role::Text,
                    "GAME",
                    x + 12.0,
                    604.0,
                    card_w - 24.0,
                    24.0,
                    plate_token(stable_plate(id)),
                ),
                node(
                    &format!("label-{id}"),
                    Role::Text,
                    title,
                    x,
                    648.0,
                    card_w,
                    28.0,
                    if index == self.focus {
                        "--state-focused-text"
                    } else {
                        "--color-text-secondary"
                    },
                ),
            ];
            children.push(card);
        }
        children.extend([node(
            "prompts",
            Role::Text,
            footer_prompt,
            w - 560.0,
            h - 48.0,
            512.0,
            32.0,
            "--color-text-secondary",
        )]);
        let root = Node::new(
            NodeId::new("quiet-console").unwrap(),
            Role::Group,
            "Quiet Console Home",
            Bounds::new(0.0, 0.0, w, h),
            "--color-surface-canvas",
        )
        .with_children(children);
        let default = self
            .ready
            .get(self.focus)
            .map_or("quiet-console".to_owned(), |(id, _)| format!("ready-{id}"));
        Scene::new(root, NodeId::new(default).unwrap()).expect("unique deterministic Home scene")
    }
}

fn stable_plate(id: &str) -> usize {
    id.bytes().fold(0_usize, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(usize::from(byte))
    })
}

fn plate_token(hash: usize) -> &'static str {
    [
        "--deco-plate-a-bg",
        "--deco-plate-b-bg",
        "--deco-plate-c-bg",
        "--deco-plate-d-bg",
        "--deco-plate-e-bg",
        "--deco-plate-f-bg",
    ][hash % 6]
}

fn node(id: &str, role: Role, label: &str, x: f32, y: f32, w: f32, h: f32, token: &str) -> Node {
    Node::new(
        NodeId::new(id).unwrap(),
        role,
        label,
        Bounds::new(x, y, w, h),
        token,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_catalog::{
        AppKind, AppManifestRef, Presentation as CatalogPresentation, Provenance, UserProjection,
        Variant,
    };
    use pf_ports::{FakeSession, ScriptedSession};
    use std::path::PathBuf;

    fn snapshot() -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            user_projection: UserProjection::default(),
            items: ["Ridgeline", "Hollow Tides", "Sunwake"]
                .into_iter()
                .enumerate()
                .map(|(i, title)| pf_catalog::CatalogItem {
                    id: format!("app-{i}"),
                    title: title.into(),
                    kind: AppKind::Game,
                    presentation: CatalogPresentation {
                        icon_reference: None,
                    },
                    tags: vec![],
                    variants: vec![Variant {
                        id: "default".into(),
                        provider_id: "fixture".into(),
                        availability: Availability::Ready,
                        requirements: vec![],
                        provenance: Provenance {
                            provider_id: "fixture".into(),
                            app_version: None,
                            upstream_version: None,
                            runtime_family: "native".into(),
                            runtime_abi: "aarch64".into(),
                            platform_version: None,
                        },
                        launch_target: AppManifestRef {
                            app_id: format!("app-{i}"),
                            descriptor_path: PathBuf::from("fixture.toml"),
                            observed_digest: "fixture".into(),
                        },
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn ready_now_selects_and_launches_first_ready_variant() {
        let mut snapshot = snapshot();
        let mut item = snapshot.items[0].clone();
        item.id = "app-multi".into();
        item.title = "Glass Harbor".into();
        item.variants[0].id = "setup-first".into();
        item.variants[0].availability = Availability::NeedsSetup {
            reason: "fixture".into(),
        };
        let mut ready = item.variants[0].clone();
        ready.id = "ready-second".into();
        ready.availability = Availability::Ready;
        ready.launch_target.app_id = "glass-harbor-ready".into();
        item.variants.push(ready);
        snapshot.items = vec![item];

        let mut core = ShellCore::boot(&snapshot, &pf_theme::flagship(), false);
        assert_eq!(
            core.action(&ShellAction::Activate),
            Some(Effect::Launch(LaunchRequest {
                item_id: "glass-harbor-ready".into(),
            }))
        );
    }

    #[test]
    fn transcript_restores_focus_without_interstitial() {
        let mut core = ShellCore::boot(&snapshot(), &pf_theme::flagship(), false);
        core.action(&ShellAction::Move(AxisMove::Right));
        let effect = core.action(&ShellAction::Activate).unwrap();
        assert!(matches!(effect, Effect::Launch(_)));
        let mut fake = FakeSession::new(
            Ok(LaunchResult::Accepted {
                session_id: "fake-1".into(),
            }),
            [
                ScriptedSession::Event(SessionEvent::Observed(ObservedSessionState::Running)),
                ScriptedSession::Event(SessionEvent::Observed(
                    ObservedSessionState::ObservationComplete,
                )),
                ScriptedSession::Event(SessionEvent::Terminal(TerminalReceipt::Returned {
                    session_id: "fake-1".into(),
                })),
                ScriptedSession::Idle,
            ],
        );
        let result = fake
            .launch(match effect {
                Effect::Launch(request) => request,
                Effect::SafeReturn => unreachable!(),
            })
            .unwrap();
        core.launch_result(&result);
        core.drive_session(&mut fake).unwrap();
        assert_eq!(
            (core.route(), core.presentation(), core.focus()),
            (Route::Home, Presentation::Home, 1)
        );
    }

    #[test]
    fn reduced_motion_is_structural_stop() {
        let core = ShellCore::boot(&snapshot(), &pf_theme::flagship(), true);
        assert_eq!(core.motion_duration_ms(), 0);
    }

    #[test]
    fn safe_return_is_routed_from_every_presentation_state() {
        let safe_return = ShellAction::Custom("SafeReturn".into());
        let mut core = ShellCore::boot(&snapshot(), &pf_theme::flagship(), false);
        assert_eq!(core.action(&safe_return), Some(Effect::SafeReturn));

        let launch = core.action(&ShellAction::Activate).unwrap();
        assert!(matches!(launch, Effect::Launch(_)));
        assert_eq!(core.presentation(), Presentation::LaunchDimmed);
        assert_eq!(core.action(&safe_return), Some(Effect::SafeReturn));

        core.launch_result(&LaunchResult::Accepted {
            session_id: "safe-return-transcript".into(),
        });
        assert_eq!(core.presentation(), Presentation::AppRunning);
        assert_eq!(core.action(&safe_return), Some(Effect::SafeReturn));
    }
}
