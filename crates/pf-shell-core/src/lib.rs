//! Product shell state/event/effect reducer. Runtime lifecycle remains authority-owned.
#![allow(
    clippy::missing_panics_doc,
    clippy::missing_errors_doc,
    clippy::struct_excessive_bools,
    clippy::semicolon_if_nothing_returned,
    clippy::cast_precision_loss,
    clippy::items_after_statements,
    clippy::default_trait_access,
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
    Library,
    Settings,
    Quick,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Presentation {
    Booting,
    Ready,
    Starting,
    Running,
    Returned,
    ForcedClose,
    Crash,
    RecoveryRequired,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Effect {
    Launch(LaunchRequest),
    SafeReturn,
    EnterRecovery,
}

#[derive(Clone, Debug)]
struct Item {
    id: String,
    title: String,
    availability: Availability,
}

pub struct ShellCore {
    route: Route,
    previous_route: Route,
    presentation: Presentation,
    items: Vec<Item>,
    focus: usize,
    saved_focus: [usize; 4],
    launch_focus: usize,
    active_title: String,
    crash_summary: String,
    recovery_available: bool,
    pending_ack: bool,
    just_returned: bool,
    motion_ms: u32,
    reduced_motion: bool,
}

impl ShellCore {
    #[must_use]
    pub fn boot(snapshot: &CatalogSnapshot, theme: &Theme, reduced_motion: bool) -> Self {
        let items = snapshot
            .items
            .iter()
            .map(|item| Item {
                id: item
                    .variants
                    .first()
                    .map_or_else(|| item.id.clone(), |v| v.launch_target.app_id.clone()),
                title: item.title.clone(),
                availability: item.variants.first().map_or(
                    Availability::NeedsSetup {
                        reason: "No launch route".into(),
                    },
                    |v| v.availability.clone(),
                ),
            })
            .collect();
        Self {
            route: Route::Home,
            previous_route: Route::Home,
            presentation: Presentation::Booting,
            items,
            focus: 0,
            saved_focus: [0; 4],
            launch_focus: 0,
            active_title: String::new(),
            crash_summary: String::new(),
            recovery_available: false,
            pending_ack: false,
            just_returned: false,
            motion_ms: theme
                .resolve_motion("launch", reduced_motion)
                .expect("motion.launch")
                .duration_ms,
            reduced_motion,
        }
    }

    pub fn authority_snapshot(&mut self, recovery_available: bool) {
        self.recovery_available = recovery_available;
        if self.presentation == Presentation::Booting {
            self.presentation = Presentation::Ready;
        }
    }
    #[must_use]
    pub const fn route(&self) -> Route {
        self.route
    }
    #[must_use]
    pub const fn presentation(&self) -> &Presentation {
        &self.presentation
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
    #[must_use]
    pub const fn recovery_available(&self) -> bool {
        self.recovery_available
    }
    #[must_use]
    pub const fn needs_presentation_ack(&self) -> bool {
        self.pending_ack
    }
    #[must_use]
    pub const fn has_shell_frame(&self) -> bool {
        !matches!(
            self.presentation,
            Presentation::Running | Presentation::RecoveryRequired
        )
    }

    pub fn acknowledge_presentation(&mut self) -> bool {
        std::mem::take(&mut self.pending_ack)
    }

    pub fn action(&mut self, action: &ShellAction) -> Option<Effect> {
        if matches!(action, ShellAction::Custom(name) if name == "SafeReturn") {
            return Some(Effect::SafeReturn);
        }
        if !self.has_shell_frame() {
            return None;
        }
        if matches!(self.presentation, Presentation::Crash) {
            return match action {
                ShellAction::Back | ShellAction::Activate => {
                    self.presentation = Presentation::Ready;
                    self.go(Route::Home);
                    None
                }
                ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                    self.focus = 1;
                    None
                }
                ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                    self.focus = 0;
                    None
                }
                ShellAction::Custom(_) => None,
            };
        }
        match action {
            ShellAction::Custom(name) if name == "Quick" => {
                self.go(Route::Quick);
            }
            ShellAction::Back if self.route == Route::Quick => {
                let route = self.previous_route;
                self.go(route);
            }
            ShellAction::Back if self.route != Route::Home => self.go(Route::Home),
            ShellAction::Move(AxisMove::Right) if self.route == Route::Home => {
                self.go(Route::Library)
            }
            ShellAction::Move(AxisMove::Right) if self.route == Route::Library => {
                self.go(Route::Settings)
            }
            ShellAction::Move(AxisMove::Left) if self.route == Route::Settings => {
                self.go(Route::Library)
            }
            ShellAction::Move(AxisMove::Left) if self.route == Route::Library => {
                self.go(Route::Home)
            }
            ShellAction::Move(AxisMove::Down | AxisMove::Right) => {
                self.focus = (self.focus + 1).min(self.focus_count().saturating_sub(1))
            }
            ShellAction::Move(AxisMove::Up | AxisMove::Left) => {
                self.focus = self.focus.saturating_sub(1)
            }
            ShellAction::Activate => return self.activate(),
            ShellAction::Back | ShellAction::Custom(_) => {}
        }
        None
    }

    fn activate(&mut self) -> Option<Effect> {
        if self.route == Route::Settings
            && self.recovery_available
            && self.focus + 1 == self.focus_count()
        {
            return Some(Effect::EnterRecovery);
        }
        if self.route == Route::Quick {
            return match self.focus {
                0 => {
                    self.go(self.previous_route);
                    self.activate()
                }
                1 => {
                    self.go(Route::Library);
                    None
                }
                _ => None,
            };
        }
        if self.route != Route::Home {
            return None;
        }
        let item = self.items.get(self.focus)?;
        if !matches!(item.availability, Availability::Ready) {
            return None;
        }
        self.launch_focus = self.focus;
        self.active_title.clone_from(&item.title);
        self.presentation = Presentation::Starting;
        Some(Effect::Launch(LaunchRequest {
            item_id: item.id.clone(),
        }))
    }

    fn go(&mut self, route: Route) {
        self.saved_focus[self.route_index()] = self.focus;
        if route == Route::Quick {
            self.previous_route = self.route;
        }
        self.route = route;
        self.focus = self.saved_focus[self.route_index()].min(self.focus_count().saturating_sub(1));
    }
    fn route_index(&self) -> usize {
        match self.route {
            Route::Home => 0,
            Route::Library => 1,
            Route::Settings => 2,
            Route::Quick => 3,
        }
    }
    fn focus_count(&self) -> usize {
        match self.route {
            Route::Home => self.items.len().max(1),
            Route::Library => 1,
            Route::Settings => 1 + usize::from(self.recovery_available),
            Route::Quick => 2,
        }
    }

    pub fn launch_result(&mut self, result: &LaunchResult) {
        match result {
            LaunchResult::Accepted { .. } => self.presentation = Presentation::Starting,
            _ => self.presentation = Presentation::Ready,
        }
    }
    pub fn session_event(&mut self, event: &SessionEvent) {
        match event {
            SessionEvent::Observed(ObservedSessionState::Starting) => {
                self.presentation = Presentation::Starting
            }
            SessionEvent::Observed(ObservedSessionState::Running) => {
                self.presentation = Presentation::Running
            }
            SessionEvent::Terminal(TerminalReceipt::Returned { .. }) => {
                self.presentation = Presentation::Returned;
                self.focus = self.launch_focus;
                self.just_returned = true;
                self.pending_ack = true;
            }
            SessionEvent::Terminal(TerminalReceipt::ForcedClose { .. }) => {
                self.presentation = Presentation::ForcedClose;
                self.focus = self.launch_focus;
                self.pending_ack = true;
            }
            SessionEvent::Terminal(TerminalReceipt::Crash { summary, .. }) => {
                self.presentation = Presentation::Crash;
                self.crash_summary.clone_from(summary);
                self.focus = 0;
                self.pending_ack = true;
            }
            SessionEvent::RecoveryRequired(_) => {
                self.presentation = Presentation::RecoveryRequired;
                self.pending_ack = false;
            }
            SessionEvent::Observed(
                ObservedSessionState::Suspended | ObservedSessionState::ObservationComplete,
            ) => {}
        }
    }

    pub fn drive_session(
        &mut self,
        port: &mut dyn SessionPort,
    ) -> Result<(), pf_ports::SessionError> {
        while let SessionPoll::Event(event) = port.next_event(Deadline(MonotonicTime::ZERO))? {
            self.session_event(&event);
            if matches!(self.presentation, Presentation::RecoveryRequired) {
                break;
            }
        }
        Ok(())
    }

    #[must_use]
    pub fn scene(&self, metrics: SurfaceMetrics, footer: &str) -> Option<Scene> {
        if !self.has_shell_frame() {
            return None;
        }
        let (w, h) = (metrics.logical_width, metrics.logical_height);
        let mut children = vec![node(
            "rooms",
            Role::Text,
            "L     Home     Library     Settings     R",
            w / 2.0 - 220.0,
            16.0,
            440.0,
            32.0,
            "--state-rest-text",
        )];
        match self.presentation {
            Presentation::Crash => self.crash_nodes(&mut children, w, h),
            _ if self.route == Route::Quick => self.quick_nodes(&mut children, w, h),
            _ => self.route_nodes(&mut children, w, h),
        }
        children.push(node(
            "prompts",
            Role::Text,
            footer,
            w - 600.0,
            h - 48.0,
            552.0,
            32.0,
            "--color-text-secondary",
        ));
        let focus_id = children
            .iter()
            .find(|n| n.state.focused)
            .map_or("quiet-console", |n| n.id.as_str())
            .to_owned();
        let root = Node::new(
            NodeId::new("quiet-console").unwrap(),
            Role::Group,
            "Quiet Console",
            Bounds::new(0.0, 0.0, w, h),
            "--color-surface-canvas",
        )
        .with_children(children);
        Some(
            Scene::new(root, NodeId::new(focus_id).unwrap())
                .expect("one deterministic focus owner"),
        )
    }

    fn route_nodes(&self, out: &mut Vec<Node>, w: f32, _h: f32) {
        let heading = match self.route {
            Route::Home => {
                if self.just_returned {
                    "RECENT · JUST NOW"
                } else {
                    "RECENT · TONIGHT"
                }
            }
            Route::Library => "LIBRARY",
            Route::Settings => "SETTINGS",
            Route::Quick => unreachable!(),
        };
        out.push(node(
            "route-heading",
            Role::Heading,
            heading,
            48.0,
            112.0,
            500.0,
            48.0,
            "--state-rest-text",
        ));
        if self.route == Route::Home {
            for (i, item) in self.items.iter().enumerate() {
                let status = availability_text(&item.availability, &self.presentation);
                let mut n = node(
                    &format!("item-{}", item.id),
                    Role::Button,
                    &format!("{} — {status}", item.title),
                    48.0 + i as f32 * 210.0,
                    210.0,
                    190.0,
                    250.0,
                    state_token(&item.availability, i == self.focus),
                );
                n.action = Some(NodeAction::Activate);
                n.state.focused = i == self.focus;
                n.state.disabled = !matches!(item.availability, Availability::Ready);
                out.push(n);
            }
            if self.presentation == Presentation::ForcedClose {
                out.push(node(
                    "attention",
                    Role::Text,
                    &format!("Attention · {} didn't close cleanly", self.active_title),
                    48.0,
                    500.0,
                    w - 96.0,
                    42.0,
                    "--color-status-attention",
                ));
            }
        } else {
            let labels: Vec<&str> = if self.route == Route::Library {
                vec!["Browse the library — details and search arrive in Library"]
            } else if self.recovery_available {
                vec![
                    "Accessibility and controls arrive in Settings",
                    "Open independent recovery",
                ]
            } else {
                vec!["Accessibility and controls arrive in Settings"]
            };
            for (i, label) in labels.iter().enumerate() {
                let mut n = node(
                    &format!("link-{i}"),
                    Role::Button,
                    label,
                    48.0,
                    190.0 + i as f32 * 70.0,
                    w - 96.0,
                    54.0,
                    if i == self.focus {
                        "--state-focused-ring"
                    } else {
                        "--state-rest-surface"
                    },
                );
                n.state.focused = i == self.focus;
                n.action = Some(NodeAction::Activate);
                out.push(n);
            }
        }
    }
    fn quick_nodes(&self, out: &mut Vec<Node>, w: f32, h: f32) {
        // Intentionally no title: §4.2/§4.7 makes the first contextual action the top edge.
        for (i, label) in ["Open focused item", "Browse the library"]
            .iter()
            .enumerate()
        {
            let mut n = node(
                &format!("quick-{i}"),
                Role::Button,
                label,
                w - 400.0,
                96.0 + i as f32 * 64.0,
                352.0,
                52.0,
                if i == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
        out.push(node(
            "quick-truth",
            Role::Text,
            "Nothing is running now. Quick shows only what applies right here.",
            w - 400.0,
            h - 110.0,
            352.0,
            60.0,
            "--color-text-secondary",
        ));
    }
    fn crash_nodes(&self, out: &mut Vec<Node>, w: f32, _h: f32) {
        out.push(node(
            "crash-eyebrow",
            Role::Text,
            "⚠ Closed unexpectedly",
            180.0,
            100.0,
            w - 360.0,
            40.0,
            "--color-status-attention",
        ));
        out.push(node(
            "crash-title",
            Role::Heading,
            &self.active_title,
            180.0,
            150.0,
            w - 360.0,
            54.0,
            "--state-rest-text",
        ));
        out.push(node("crash-copy", Role::Text, &format!("{} stopped on its own and the shelf took the screen back. Nothing else was affected, and it's ready to open again.", self.active_title), 180.0, 220.0, w - 360.0, 70.0, "--color-text-secondary"));
        out.push(node(
            "crash-facts",
            Role::Text,
            &format!("Session · Ended · What happened · {}", self.crash_summary),
            180.0,
            310.0,
            w - 360.0,
            50.0,
            "--color-status-attention",
        ));
        out.push(node("crash-honesty", Role::Text, "This record stays on the device — there's nowhere it gets sent, so there's no Report button to press.", 180.0, 380.0, w - 360.0, 60.0, "--color-text-secondary"));
        for (i, label) in ["Back to Home", "Open again"].iter().enumerate() {
            let mut n = node(
                &format!("crash-action-{i}"),
                Role::Button,
                label,
                180.0,
                480.0 + i as f32 * 62.0,
                360.0,
                50.0,
                if i == self.focus {
                    "--state-focused-ring"
                } else {
                    "--state-rest-surface"
                },
            );
            n.state.focused = i == self.focus;
            n.action = Some(NodeAction::Activate);
            out.push(n);
        }
    }
}

fn availability_text(a: &Availability, p: &Presentation) -> String {
    match a {
        Availability::Ready if matches!(p, Presentation::Starting) => "Starting".into(),
        Availability::Ready => "Ready".into(),
        Availability::NeedsNetwork { reason } => format!("Network required — {reason}"),
        Availability::NeedsSetup { reason } => format!("Finish setup — {reason}"),
        Availability::UnsupportedCapability { capability } => {
            format!("Not supported on this device — {capability}")
        }
        Availability::IncompatibleRuntime {
            required,
            available,
        } => format!("Not supported — requires {required}; found {available}"),
    }
}
fn state_token(a: &Availability, focused: bool) -> &'static str {
    if focused {
        "--state-focused-ring"
    } else {
        match a {
            Availability::Ready => "--state-rest-surface",
            Availability::NeedsNetwork { .. } | Availability::NeedsSetup { .. } => {
                "--state-attention-surface"
            }
            Availability::UnsupportedCapability { .. }
            | Availability::IncompatibleRuntime { .. } => "--state-unavailable-surface",
        }
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
        AppKind, AppManifestRef, Presentation as CP, Provenance, UserProjection, Variant,
    };
    use std::path::PathBuf;
    fn snapshot() -> CatalogSnapshot {
        CatalogSnapshot {
            revision: 1,
            observed_at_unix_seconds: 0,
            provider_results: vec![],
            user_projection: UserProjection::default(),
            items: ["Ridgeline", "Hollow Tides"]
                .into_iter()
                .enumerate()
                .map(|(i, t)| pf_catalog::CatalogItem {
                    id: format!("i{i}"),
                    title: t.into(),
                    kind: AppKind::Game,
                    presentation: CP {
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
                            descriptor_path: PathBuf::from("app.toml"),
                            observed_digest: "x".into(),
                        },
                    }],
                })
                .collect(),
        }
    }
    fn core() -> ShellCore {
        let mut c = ShellCore::boot(&snapshot(), &pf_theme::flagship(), false);
        c.authority_snapshot(false);
        c
    }
    #[test]
    fn back_restores_route_focus_and_one_owner() {
        let mut c = core();
        c.action(&ShellAction::Move(AxisMove::Down));
        c.action(&ShellAction::Custom("Quick".into()));
        c.action(&ShellAction::Back);
        assert_eq!((c.route(), c.focus()), (Route::Home, 1));
        let s = c
            .scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape,
                },
                "",
            )
            .unwrap();
        fn count(n: &Node) -> usize {
            usize::from(n.state.focused) + n.children.iter().map(count).sum::<usize>()
        }
        assert_eq!(count(s.root()), 1);
    }
    #[test]
    fn all_presentations_route_safe_return() {
        let mut c = core();
        for p in [
            Presentation::Ready,
            Presentation::Starting,
            Presentation::Running,
            Presentation::Returned,
            Presentation::ForcedClose,
            Presentation::Crash,
            Presentation::RecoveryRequired,
        ] {
            c.presentation = p;
            assert_eq!(
                c.action(&ShellAction::Custom("SafeReturn".into())),
                Some(Effect::SafeReturn)
            );
        }
    }
    #[test]
    fn receipts_wait_for_ack_and_recovery_has_no_frame() {
        let mut c = core();
        c.session_event(&SessionEvent::Terminal(TerminalReceipt::Crash {
            session_id: "s".into(),
            summary: "exit status 9".into(),
        }));
        assert!(c.needs_presentation_ack());
        assert!(
            c.scene(
                SurfaceMetrics {
                    logical_width: 1280.,
                    logical_height: 720.,
                    scale: 1.,
                    safe_insets: Default::default(),
                    orientation: pf_scene::Orientation::Landscape
                },
                ""
            )
            .is_some()
        );
        assert!(c.acknowledge_presentation());
        c.session_event(&SessionEvent::RecoveryRequired(
            pf_ports::RecoveryRequired {
                session_id: "s".into(),
                reason: "owner unavailable".into(),
            },
        ));
        assert!(!c.has_shell_frame());
    }
    #[test]
    fn recovery_entry_is_authority_gated() {
        let mut c = core();
        c.go(Route::Settings);
        assert_eq!(c.focus_count(), 1);
        c.authority_snapshot(true);
        assert_eq!(c.focus_count(), 2);
        c.focus = 1;
        assert_eq!(
            c.action(&ShellAction::Activate),
            Some(Effect::EnterRecovery)
        );
    }
}
