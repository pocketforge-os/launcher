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
            .filter(|item| {
                item.variants.len() == 1
                    && matches!(item.variants[0].availability, Availability::Ready)
            })
            .map(|item| (item.id.clone(), item.title.clone()))
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
            ShellAction::Custom(name) if name == "SafeReturn" => return Some(Effect::SafeReturn),
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
    pub fn scene(&self, metrics: SurfaceMetrics, activate_prompt: &str) -> Scene {
        let w = metrics.logical_width;
        let h = metrics.logical_height;
        let margin = (w * 0.05).max(32.0);
        let dimmed = self.presentation != Presentation::Home;
        let mut children = vec![
            node(
                "brand",
                Role::Text,
                "POCKETFORGE",
                margin,
                22.0,
                180.0,
                32.0,
                "--state-rest-text",
            ),
            node(
                "home",
                Role::Heading,
                "HOME",
                margin,
                78.0,
                w * 0.55,
                76.0,
                "--state-rest-text",
            ),
            node(
                "status",
                Role::Text,
                if dimmed {
                    "LIGHTS OUT"
                } else {
                    "YOUR GAMES, READY WHEN YOU ARE"
                },
                margin,
                154.0,
                w * 0.7,
                38.0,
                "--color-text-muted",
            ),
            node(
                "ready-heading",
                Role::Heading,
                "READY NOW",
                margin,
                h * 0.39,
                220.0,
                36.0,
                "--state-rest-text",
            ),
        ];
        let count = self.ready.len().max(1) as f32;
        let gap = 18.0;
        let card_w = ((w - margin * 2.0 - gap * (count - 1.0)) / count).min(290.0);
        for (index, (id, title)) in self.ready.iter().enumerate() {
            let mut card = node(
                &format!("ready-{id}"),
                Role::Button,
                title,
                margin + index as f32 * (card_w + gap),
                h * 0.47,
                card_w,
                h * 0.29,
                if index == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            card.action = Some(NodeAction::Activate);
            card.state.focused = index == self.focus;
            card.state.disabled = dimmed;
            children.push(card);
        }
        children.extend([
            node(
                "nav",
                Role::Text,
                "HOME    QUICK · COMING LATER    LIBRARY · COMING LATER    SETTINGS · COMING LATER",
                margin,
                h - 82.0,
                w - margin * 2.0,
                28.0,
                "--state-rest-text",
            ),
            node(
                "prompts",
                Role::Text,
                &format!("{activate_prompt}  ACTIVATE     B  BACK"),
                margin,
                h - 46.0,
                w - margin * 2.0,
                24.0,
                "--color-text-muted",
            ),
        ]);
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
            items: ["Celeste", "Sonic Mania", "A Short Hike"]
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
}
