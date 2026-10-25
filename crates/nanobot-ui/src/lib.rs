//! nanobot-ui: the gpui workbench shell (design 4.8).
//!
//! The shell layer's generic half. A harness workbench is a **session list** plus
//! a **session workspace** that switches as a whole — conversation stream, plan
//! review, the approval / clarify bars, the composer — with a viewport region and
//! a panel dock on top. The shell is generic over [`model::SessionView`], so the
//! product keeps its own session type, workspace projection and renderer:
//!
//! ```text
//! nanobot-ui (shell)
//!   ├─ built-in surfaces: session list / session workspace / conversation /
//!   │                      plan review / approval bar / dock regions
//!   └─ plugin surfaces: script panels and views mounted from nanobot-rhai
//! ```
//!
//! The product registers its domain panels through
//! [`shell::HarnessShell::panel_host`] and its renderer through
//! [`nanobot_gpu::ViewportSlot`]. Nothing in this crate knows a domain word.
//!
//! The shell's chrome (dark/light theme, the settings panel) is framework
//! surface: [`theme`] switches the gpui-component global theme every palette
//! read derives from, and [`shell::HarnessShell::set_settings`] registers the
//! settings panel that persists the choice through `ui.theme`.

pub mod model;
pub mod panels;
pub mod poll;
pub mod shell;
pub mod theme;
pub mod widgets;

pub use model::{
    plan_progress, resolve_sidebar_groups, Block, BlockKind, BranchMarker, Conversation,
    PlanDecision, PlanPhase, PlanStep, Project, ProjectId, SessionId, SessionStatus, SessionStore,
    SessionView, SidebarGroup, SidebarProjection, StepStatus, ToolState, WorkspaceView,
};
pub use panels::{PanelArea, PanelDef, PanelHost, PanelSource, ScriptRuntime};
pub use poll::poll_periodically;
pub use shell::HarnessShell;
pub use theme::{apply, apply_from_settings, set_theme, ThemeKind, THEME_KEY};
pub use widgets::{button, line, picker, short_hash, show, truncate, HashedRef};

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AnyElement, IntoElement as _};
    use nanobot_gpu::{ViewportInput, ViewportProjection, ViewportSlot};
    use std::cell::RefCell;
    use std::rc::Rc;

    // ---- a second consumer: proves the contract is implementable ----

    #[derive(Clone)]
    struct TestSession {
        id: SessionId,
        title: String,
        status: SessionStatus,
        conversation: Conversation,
        plan: Vec<PlanStep>,
        plan_phase: PlanPhase,
        clarify_questions: Vec<String>,
        parent_session: Option<SessionId>,
        parent_seq: Option<u32>,
    }

    impl TestSession {
        fn new(id: &str) -> Self {
            Self {
                id: SessionId::new(id),
                title: format!("session {id}"),
                status: SessionStatus::Active,
                conversation: Conversation::default(),
                plan: Vec::new(),
                plan_phase: PlanPhase::None,
                clarify_questions: Vec::new(),
                parent_session: None,
                parent_seq: None,
            }
        }
    }

    #[derive(Clone)]
    struct TestWorkspace {
        session_id: SessionId,
        title: String,
        status: SessionStatus,
        conversation: Conversation,
        plan: Vec<PlanStep>,
        plan_phase: PlanPhase,
        clarify_questions: Vec<String>,
        progress: Option<(u32, u32)>,
        settlement: Vec<(String, bool)>,
    }

    impl WorkspaceView for TestWorkspace {
        fn session_id(&self) -> &SessionId {
            &self.session_id
        }
        fn title(&self) -> String {
            self.title.clone()
        }
        fn status(&self) -> SessionStatus {
            self.status
        }
        fn conversation(&self) -> Conversation {
            self.conversation.clone()
        }
        fn plan(&self) -> Vec<PlanStep> {
            self.plan.clone()
        }
        fn plan_phase(&self) -> PlanPhase {
            self.plan_phase
        }
        fn clarify_questions(&self) -> Vec<String> {
            self.clarify_questions.clone()
        }
        fn progress(&self) -> Option<(u32, u32)> {
            self.progress
        }
        fn set_progress(&mut self, progress: Option<(u32, u32)>) {
            self.progress = progress;
        }
        fn settlement_items(&self) -> Vec<(String, bool)> {
            self.settlement.clone()
        }
    }

    #[derive(Default)]
    struct TestViewportHandle {
        frames: u64,
    }

    struct TestScene;

    struct TestViewport;

    impl ViewportSlot for TestViewport {
        type Handle = Rc<RefCell<TestViewportHandle>>;
        type Scene = TestScene;

        fn new_handle() -> Self::Handle {
            Rc::new(RefCell::new(TestViewportHandle::default()))
        }
        fn feed_input(_handle: &Self::Handle, _input: ViewportInput) {}
        fn update_scene(handle: &Self::Handle, _scene: &Self::Scene) {
            handle.borrow_mut().frames += 1;
        }
        fn projection(_handle: &Self::Handle) -> Option<ViewportProjection> {
            Some(ViewportProjection::Orthographic)
        }
        fn painted_frames(handle: &Self::Handle) -> u64 {
            handle.borrow().frames
        }
        fn element(_handle: &Self::Handle) -> AnyElement {
            gpui::div().into_any_element()
        }
    }

    impl SessionView for TestSession {
        type Scene = TestScene;
        type Viewport = TestViewport;
        type Workspace = TestWorkspace;

        fn id(&self) -> &SessionId {
            &self.id
        }
        fn title(&self) -> String {
            self.title.clone()
        }
        fn status(&self) -> SessionStatus {
            self.status
        }
        fn conversation(&self) -> Conversation {
            self.conversation.clone()
        }
        fn plan(&self) -> Vec<PlanStep> {
            self.plan.clone()
        }
        fn plan_phase(&self) -> PlanPhase {
            self.plan_phase
        }
        fn clarify_questions(&self) -> Vec<String> {
            self.clarify_questions.clone()
        }
        fn branch_marker(&self) -> Option<BranchMarker> {
            match (&self.parent_session, self.parent_seq) {
                (Some(parent_session), Some(parent_seq)) => Some(BranchMarker {
                    parent_session: parent_session.clone(),
                    parent_seq,
                }),
                _ => None,
            }
        }
        fn build_workspace(
            &self,
            _viewport: <Self::Viewport as ViewportSlot>::Handle,
        ) -> Self::Workspace {
            TestWorkspace {
                session_id: self.id.clone(),
                title: self.title.clone(),
                status: self.status,
                conversation: self.conversation.clone(),
                plan: self.plan.clone(),
                plan_phase: self.plan_phase,
                clarify_questions: self.clarify_questions.clone(),
                progress: None,
                settlement: Vec::new(),
            }
        }
    }

    // ---- store + shell behaviour ----

    fn project(id: &str, sessions: Vec<TestSession>, index: Option<&str>) -> Project<TestSession> {
        Project {
            id: ProjectId::new(id),
            name: format!("project {id}"),
            sessions,
            session_index: index.map(SessionId::new),
        }
    }

    fn two_project_store() -> SessionStore<TestSession> {
        SessionStore::new(
            vec![
                project(
                    "p1",
                    vec![TestSession::new("s1"), TestSession::new("s2")],
                    Some("s1"),
                ),
                project("p2", vec![TestSession::new("s3")], Some("s3")),
            ],
            ProjectId::new("p1"),
        )
    }

    /// A minimal container manifest for the `from_manifest` bridge.
    #[derive(Clone, serde::Serialize, serde::Deserialize)]
    struct TestManifest {
        info: nanobot_boot::container::ProjectInfo,
        index: nanobot_boot::container::SessionIndex,
    }

    impl nanobot_boot::container::Manifest for TestManifest {
        fn validate(&self) -> Result<(), nanobot_boot::BootError> {
            Ok(())
        }
        fn checksum(&self) -> Option<u32> {
            None
        }
        fn set_checksum(&mut self, _checksum: Option<u32>) {}
        fn sessions(&self) -> &nanobot_boot::container::SessionIndex {
            &self.index
        }
        fn sessions_mut(&mut self) -> &mut nanobot_boot::container::SessionIndex {
            &mut self.index
        }
        fn project_info(&self) -> &nanobot_boot::container::ProjectInfo {
            &self.info
        }
        fn project_info_mut(&mut self) -> &mut nanobot_boot::container::ProjectInfo {
            &mut self.info
        }
    }

    fn test_manifest(
        name: &str,
        entries: Vec<(&str, nanobot_boot::container::SessionStatus)>,
        active: Option<&str>,
    ) -> TestManifest {
        use nanobot_boot::container::{ProjectInfo, SessionEntry, SessionIndex};
        TestManifest {
            info: ProjectInfo::new(name),
            index: SessionIndex {
                active: active.map(str::to_string),
                entries: entries
                    .into_iter()
                    .map(|(id, status)| SessionEntry {
                        id: id.to_string(),
                        title: format!("title {id}"),
                        status,
                        document: None,
                    })
                    .collect(),
            },
        }
    }

    #[test]
    fn from_manifest_builds_one_project_from_the_session_index() {
        use nanobot_boot::container::{
            Manifest as _, ProjectPaths, SessionStatus as ContainerStatus,
        };
        let manifest = test_manifest(
            "demo",
            vec![
                ("s1", ContainerStatus::Active),
                ("s2", ContainerStatus::AwaitingReview),
            ],
            Some("s2"),
        );
        let paths = ProjectPaths::new("/tmp/does-not-exist");
        let store = SessionStore::from_manifest(&manifest, &paths, |entry, _| {
            let mut session = TestSession::new(&entry.id);
            session.title = entry.title.clone();
            session.status = SessionStatus::from(entry.status);
            session
        });

        assert_eq!(
            store.open_project_id(),
            &ProjectId::new(manifest.project_info().uuid.clone()),
            "the project's id is the container's uuid"
        );
        assert_eq!(store.open_project().name, "demo");
        let sessions = store.open_project_sessions();
        assert_eq!(
            sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![SessionId::new("s1"), SessionId::new("s2")],
            "entry order is the index's"
        );
        assert_eq!(sessions[1].status, SessionStatus::AwaitingReview);
        assert_eq!(
            store.open_project().session_index,
            Some(SessionId::new("s2")),
            "the index's active session is restored"
        );
    }

    #[test]
    fn without_a_projection_the_sidebar_is_one_flat_group() {
        let store = two_project_store();
        let groups = resolve_sidebar_groups(&store, None);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "project p1");
        assert_eq!(
            groups[0].1.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![SessionId::new("s1"), SessionId::new("s2")]
        );
    }

    #[test]
    fn a_projection_groups_the_sidebar_and_strays_stay_reachable() {
        use std::rc::Rc;

        struct DesignRooted;
        impl SidebarProjection<TestSession> for DesignRooted {
            fn groups(&self, store: &SessionStore<TestSession>) -> Vec<SidebarGroup> {
                // Deliberately partial and order-shuffled: s2 under "design
                // beta", s1 never named, an unknown id dropped.
                let _ = store;
                vec![
                    SidebarGroup::new("design beta", vec![SessionId::new("s2")]),
                    SidebarGroup::new("design gamma", vec![SessionId::new("gone")]),
                ]
            }
        }

        let store = two_project_store();
        let groups = resolve_sidebar_groups(
            &store,
            Some(&DesignRooted as &dyn SidebarProjection<TestSession>),
        );
        let titles = groups
            .iter()
            .map(|(label, _)| label.clone())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["design beta", "其他"]);
        assert_eq!(
            groups[0].1.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![SessionId::new("s2")]
        );
        assert_eq!(
            groups[1].1.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![SessionId::new("s1")],
            "the projection is a view, not a strainer"
        );
        let _ = Rc::new(());
    }

    #[test]
    fn from_manifest_opens_an_empty_index_as_an_empty_project() {
        use nanobot_boot::container::ProjectPaths;
        let manifest = test_manifest("empty", Vec::new(), None);
        let paths = ProjectPaths::new("/tmp/does-not-exist");
        let store =
            SessionStore::from_manifest(&manifest, &paths, |entry, _| TestSession::new(&entry.id));
        assert!(store.open_project_sessions().is_empty());
        assert_eq!(store.open_project().session_index, None);
    }

    #[test]
    fn the_shell_opens_the_project_at_its_session_index() {
        let shell = HarnessShell::new(two_project_store());
        assert_eq!(shell.store().open_project_id(), &ProjectId::new("p1"));
        assert_eq!(
            shell.active_session_id(),
            Some(&SessionId::new("s1")),
            "the session index is the one presented"
        );
        assert_eq!(shell.workspace().map(|ws| ws.title()), Some("session s1".to_string()));
    }

    #[test]
    fn switching_sessions_rebuilds_the_workspace_and_keeps_each_viewport() {
        let mut shell = HarnessShell::new(two_project_store());
        let first = shell.viewport_of(&SessionId::new("s1"));
        assert!(shell.switch_session(&SessionId::new("s2")));
        assert_eq!(shell.active_session_id(), Some(&SessionId::new("s2")));
        let second = shell.viewport_of(&SessionId::new("s2"));
        assert!(!Rc::ptr_eq(&first, &second), "each session has its own viewport");
        assert!(shell.switch_session(&SessionId::new("s1")));
        let restored = shell.viewport_of(&SessionId::new("s1"));
        assert!(
            Rc::ptr_eq(&restored, &first),
            "switching back restores the first session's own viewport"
        );
        assert_eq!(shell.store().open_project().session_index, Some(SessionId::new("s1")));
    }

    #[test]
    fn switching_projects_replaces_the_whole_workspace() {
        let mut shell = HarnessShell::new(two_project_store());
        assert!(shell.switch_project(&ProjectId::new("p2")));
        assert_eq!(shell.store().open_project_id(), &ProjectId::new("p2"));
        assert_eq!(shell.active_session_id(), Some(&SessionId::new("s3")));
        // The other project's session is not addressable while p2 is open.
        assert!(!shell.switch_session(&SessionId::new("s1")));
        assert!(shell.switch_project(&ProjectId::new("p1")));
        assert_eq!(shell.active_session_id(), Some(&SessionId::new("s1")));
    }

    #[test]
    fn a_project_without_sessions_opens_with_an_empty_workspace() {
        let store = SessionStore::new(
            vec![project("empty", Vec::new(), None)],
            ProjectId::new("empty"),
        );
        let shell = HarnessShell::new(store);
        assert!(shell.workspace().is_none());
        assert!(shell.active_session_id().is_none());
        assert_eq!(shell.painted_frames(), 0);
    }

    #[test]
    fn update_session_content_only_touches_the_open_project() {
        let shell = HarnessShell::new(two_project_store());
        assert!(!shell.update_session_content(&SessionId::new("s3"), TestSession::new("s3")));
        assert!(shell.update_session_content(&SessionId::new("s2"), TestSession::new("s2")));
        assert_eq!(
            shell.store().session(&SessionId::new("s2")).map(|s| s.title()),
            Some("session s2".to_string())
        );
    }

    #[test]
    fn the_viewport_slot_receives_the_scene_and_reports_it() {
        let mut shell = HarnessShell::new(two_project_store());
        let handle = shell.viewport_of(&SessionId::new("s1"));
        assert_eq!(TestViewport::painted_frames(&handle), 0);
        shell.update_scene(&TestScene);
        assert_eq!(TestViewport::painted_frames(&handle), 1);
        assert_eq!(shell.painted_frames(), 1);
        assert_eq!(shell.viewport_projection(), Some(ViewportProjection::Orthographic));
    }

    #[test]
    fn set_progress_reaches_the_workspace() {
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_progress(Some((2, 5)));
        assert_eq!(shell.workspace().and_then(|ws| ws.progress()), Some((2, 5)));
        shell.set_progress(None);
        assert_eq!(shell.workspace().and_then(|ws| ws.progress()), None);
    }

    #[test]
    fn plan_and_block_vocabulary() {
        let plan = vec![
            PlanStep { text: "a".into(), status: StepStatus::Done },
            PlanStep { text: "b".into(), status: StepStatus::InProgress },
            PlanStep { text: "c".into(), status: StepStatus::Pending },
        ];
        assert_eq!(plan_progress(&plan), (1, 3));
        assert!(PlanPhase::Contingent.awaits_decision());
        assert!(PlanPhase::Clarify.awaits_decision());
        assert!(!PlanPhase::Proposed.awaits_decision());
        assert_eq!(SessionStatus::AwaitingReview.label(), "等待审查");

        let mut conversation = Conversation::default();
        conversation.blocks.push(Block::new(BlockKind::User, 1, "hi"));
        conversation.blocks.push(Block::new(BlockKind::Assistant, 2, "hello"));
        assert!(conversation.anchors_are_monotonic());
        assert_eq!(conversation.last_seq(), 2);
        conversation.blocks[1].event_seq = 1;
        assert!(!conversation.anchors_are_monotonic(), "anchors must strictly increase");
    }

    // ---- panel host ----

    fn product_panel(
        id: &'static str,
        title: &'static str,
        area: PanelArea,
        order: i32,
    ) -> PanelDef<TestSession> {
        PanelDef::product(id, title, area, order, Rc::new(|_shell, _ws, _window, _cx| {
            gpui::div().into_any_element()
        }))
    }

    #[test]
    fn panels_are_grouped_by_area_and_ordered() {
        let mut host = PanelHost::<TestSession>::default();
        host.register(product_panel("a", "A", PanelArea::Right, 2));
        host.register(product_panel("b", "B", PanelArea::Left, 1));
        host.register(product_panel("c", "C", PanelArea::Right, 1));
        assert_eq!(host.len(), 3);
        let right: Vec<String> = host
            .panels_in(PanelArea::Right)
            .into_iter()
            .map(|p| p.id.clone())
            .collect();
        assert_eq!(right, vec!["c".to_string(), "a".to_string()], "ordered by `order`");
        let left: Vec<String> = host
            .panels_in(PanelArea::Left)
            .into_iter()
            .map(|p| p.id.clone())
            .collect();
        assert_eq!(left, vec!["b".to_string()]);
        assert!(host.panels_in(PanelArea::Bottom).is_empty());
    }

    #[test]
    fn plugin_panels_are_attached_with_their_dock_area() {
        let mut host = PanelHost::<TestSession>::default();
        host.attach_plugins(&[
            nanobot_rhai::PanelRegistration {
                plugin: "com.example.demo".into(),
                id: "acme.chart".into(),
                title: "温度曲线".into(),
                script: std::path::PathBuf::from("/tmp/chart.rhai"),
                area: "right".into(),
            },
            nanobot_rhai::PanelRegistration {
                plugin: "com.example.demo".into(),
                id: "acme.log".into(),
                title: "日志".into(),
                script: std::path::PathBuf::from("/tmp/log.rhai"),
                area: "bottom".into(),
            },
            // An unknown area is skipped rather than fatal.
            nanobot_rhai::PanelRegistration {
                plugin: "com.example.demo".into(),
                id: "acme.bad".into(),
                title: "Bad".into(),
                script: std::path::PathBuf::from("/tmp/bad.rhai"),
                area: "everywhere".into(),
            },
        ]);
        assert_eq!(host.panels_in(PanelArea::Right).len(), 1);
        assert_eq!(host.panels_in(PanelArea::Bottom).len(), 1);
        assert_eq!(host.len(), 2);
        assert_eq!(PanelArea::parse("RIGHT"), Some(PanelArea::Right));
        assert_eq!(PanelArea::parse("everywhere"), None);
    }

    #[test]
    fn a_script_panel_is_an_empty_element_until_mounted() {
        let mut host = PanelHost::<TestSession>::default();
        host.register(PanelDef::script(
            "acme.chart",
            "温度曲线",
            PanelArea::Right,
            0,
            std::path::PathBuf::from("/nonexistent/chart.rhai"),
        ));
        // No window yet: the panel renders as an empty element, never a panic.
        let element = host.mounted_scripts();
        assert_eq!(element, 0);
        assert!(host.script_errors().is_empty());
    }

    #[test]
    fn sharing_settings_registers_the_framework_settings_panel_once() {
        use std::sync::{Arc, Mutex};

        let mut shell = HarnessShell::new(two_project_store());
        let before: Vec<String> = shell
            .panel_host()
            .panels_in(PanelArea::Bottom)
            .iter()
            .map(|panel| panel.id.clone())
            .collect();
        assert_eq!(
            before,
            vec!["harness.diagnostics".to_string()],
            "the diagnostics panel is framework chrome, always present"
        );

        let settings = Arc::new(Mutex::new(nanobot_boot::Settings::in_memory()));
        shell.set_settings(Some(Arc::clone(&settings)));
        let bottom: Vec<String> = shell
            .panel_host()
            .panels_in(PanelArea::Bottom)
            .iter()
            .map(|panel| panel.id.clone())
            .collect();
        assert_eq!(
            bottom,
            vec![
                "harness.settings".to_string(),
                "harness.diagnostics".to_string()
            ],
            "settings (order 0) lands before diagnostics (order 1)"
        );
        assert!(shell.settings().is_some());

        // Sharing again must not duplicate the panel.
        shell.set_settings(Some(settings));
        assert_eq!(shell.panel_host().panels_in(PanelArea::Bottom).len(), 2);

        // A stored theme name becomes the shell's tracked theme; the panel
        // highlights it on the next frame.
        let mut named = nanobot_boot::Settings::in_memory();
        named
            .set(crate::theme::THEME_KEY, serde_json::json!("light"))
            .unwrap();
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_settings(Some(Arc::new(Mutex::new(named))));
        assert_eq!(shell.theme(), crate::theme::ThemeKind::Light);
    }

    #[test]
    fn the_account_chip_shows_the_identity_or_a_guest() {
        let mut shell = HarnessShell::new(two_project_store());
        assert!(shell.account().is_none(), "a shell starts as a guest");

        shell.set_account(Some(
            nanobot_boot::Account::new("u1", "Alice").with_plan("pro"),
        ));
        assert_eq!(shell.account().map(|a| a.id.clone()), Some("u1".to_string()));
        assert_eq!(shell.account().and_then(|a| a.plan.clone()), Some("pro".to_string()));

        shell.set_account(None);
        assert!(shell.account().is_none(), "signing out clears the chip");
    }

    #[test]
    fn avatar_letters_come_from_the_display_name() {
        assert_eq!(crate::shell::avatar_letter("Alice"), 'A');
        assert_eq!(crate::shell::avatar_letter("  李四"), '李');
        assert_eq!(crate::shell::avatar_letter(""), 'U');
    }

    #[test]
    fn a_plugins_panel_registers_once_and_disable_removes_exactly_that_plugins_panels() {
        let mut shell = HarnessShell::new(two_project_store());
        shell.panel_host().register(PanelDef::product(
            "product.panel",
            "产品面板",
            PanelArea::Right,
            0,
            Rc::new(|_shell, _ws, _window, _cx| gpui::div().into_any_element()),
        ));

        let loaded = nanobot_rhai::LoadedPlugins {
            plugins_loaded: vec!["acme.chart".into(), "acme.log".into()],
            panels: vec![
                nanobot_rhai::PanelRegistration {
                    plugin: "acme.chart".into(),
                    id: "acme.chart.panel".into(),
                    title: "温度曲线".into(),
                    script: std::path::PathBuf::from("/tmp/chart.rhai"),
                    area: "right".into(),
                },
                nanobot_rhai::PanelRegistration {
                    plugin: "acme.log".into(),
                    id: "acme.log.panel".into(),
                    title: "日志".into(),
                    script: std::path::PathBuf::from("/tmp/log.rhai"),
                    area: "bottom".into(),
                },
            ],
            views: vec![],
            commands: vec![],
            toolbar: vec![],
            notes: vec![],
            plugins_rejected: vec![("acme.bad".into(), "api_version 9 not supported".into())],
        };
        // The product attaches the panels, then shares the load result.
        shell.attach_plugins(&loaded.panels);
        shell.set_plugins(&loaded);
        let bottom: Vec<String> = shell
            .panel_host()
            .panels_in(PanelArea::Bottom)
            .iter()
            .map(|panel| panel.id.clone())
            .collect();
        assert!(
            bottom.contains(&"harness.plugins".to_string()),
            "the plugins panel registered: {bottom:?}"
        );
        assert_eq!(
            shell.plugins().map(|entries| entries.to_vec()),
            Some(vec![
                crate::shell::PluginEntry { id: "acme.chart".into(), enabled: true },
                crate::shell::PluginEntry { id: "acme.log".into(), enabled: true },
            ])
        );

        // Disabling one plugin removes exactly its panel; the product panel
        // and the other plugin's panel stay.
        let removed = shell.disable_plugin("acme.chart");
        assert_eq!(removed, 1);
        let right: Vec<String> = shell
            .panel_host()
            .panels_in(PanelArea::Right)
            .iter()
            .map(|panel| panel.id.clone())
            .collect();
        assert_eq!(right, vec!["product.panel".to_string()]);
        assert!(
            shell
                .panel_host()
                .panels()
                .iter()
                .any(|panel| panel.id == "acme.log.panel"),
            "the other plugin's panel survives"
        );
        assert!(
            !shell.plugins().unwrap().iter().find(|e| e.id == "acme.chart").unwrap().enabled,
            "the disabled plugin flips its badge"
        );

        // Re-sharing keeps the disabled state; registration stays singular.
        shell.set_plugins(&loaded);
        assert_eq!(shell.panel_host().panels_in(PanelArea::Bottom).len(), 3);
        assert!(
            !shell.plugins().unwrap().iter().find(|e| e.id == "acme.chart").unwrap().enabled,
            "the user's disable survives a snapshot refresh"
        );
    }
}
