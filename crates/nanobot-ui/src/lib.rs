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
//! The shell's chrome (dark/light theme, the settings dialog) is framework
//! surface: [`theme`] switches the gpui-component global theme every palette
//! read derives from, and [`shell::HarnessShell::set_settings`] registers the
//! settings dialog that persists the choice through `ui.theme`.

pub mod activity;
pub mod composer;
pub mod commands;
pub mod i18n;
pub mod icons;
pub mod layout;
pub mod model;
pub mod model_picker;
pub mod motion;
pub mod palette;
pub mod panels;
pub mod poll;
pub mod session_menu;
pub mod settings_dialog;
pub mod shell;
pub mod sidebar;
pub mod stream;
pub mod theme;
pub mod transcript;
pub mod widgets;

pub use model::{
    is_archived, plan_progress, resolve_sidebar_groups, BranchMarker,
    PlanDecision, PlanPhase, PlanStep, Project, ProjectId, SessionId, SessionStatus, SessionStore,
    SessionView, SidebarGroup, SidebarProjection, StepStatus, WorkspaceView,
};
pub use panels::{Entitlements, PanelArea, PanelDef, PanelHost, PanelSource, ScriptRuntime};
pub use poll::poll_periodically;
pub use transcript::{
    ComposerAction, Conversation, Entry, EntryKind, SessionBadge, ToolCall, ToolKind,
    ToolStatus, Transcript, TurnPhase,
};
pub use layout::{Divider, Fold, Layout, LayoutState, Regions, Stacked};
pub use session_menu::{Chosen, SessionAction};
pub use sidebar::{SidebarRow, INACTIVE_AFTER_MS};
pub use shell::{
    init, CyclePermissionMode, HarnessShell, OpenPalette, StopTurn, ToggleDock, ToggleSidebar,
    KEY_CONTEXT, PALETTE_CONTEXT,
};
pub use stream::{Row, RowKey, RowKind, StreamState};
pub use activity::{Submitted, PERMISSION_MODE_KEY};
pub use composer::{
    Approval, ApprovalDecision, ApprovalScope, ContextLevel, Delivery, MessageQueue, PermissionMode,
    Queued, Submit,
};
pub use commands::{Candidate, Command, RewindMode, Selection, Trigger};
pub use i18n::{t, t_replace, Locale};
pub use palette::Completion;
pub use icons::{Assets, Icon};
pub use motion::{Mark, REDUCE_MOTION_KEY};
pub use theme::{
    apply, apply_font, apply_from_settings, set_theme, ThemeKind, FONT_FAMILY_KEY,
    FONT_MONO_FAMILY_KEY, FONT_SIZE_KEY, LOCALE_KEY, THEME_KEY,
};
pub use widgets::{button, line, picker, short_hash, show, truncate, HashedRef};

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AnyElement, IntoElement as _};
    use nanobot_gpu::{ViewportProjection, ViewportSlot};
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
        archived: bool,
        /// A conversation shared with whoever drives the session, so a test
        /// can stream into the workspace on screen.
        live: Option<Rc<RefCell<Conversation>>>,
    }

    impl TestSession {
        fn new(id: &str) -> Self {
            Self {
                id: SessionId::new(id),
                title: format!("session {id}"),
                status: SessionStatus::Idle,
                conversation: Conversation::default(),
                plan: Vec::new(),
                plan_phase: PlanPhase::None,
                clarify_questions: Vec::new(),
                parent_session: None,
                parent_seq: None,
                archived: false,
                live: None,
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
        live: Option<Rc<RefCell<Conversation>>>,
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
            match &self.live {
                Some(live) => live.borrow().clone(),
                None => self.conversation.clone(),
            }
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
        fn archived(&self) -> bool {
            self.archived
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
                live: self.live.clone(),
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
        assert_eq!(sessions[1].status, SessionStatus::AwaitingUser);
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
        assert_eq!(
            SessionStatus::AwaitingUser.label(crate::i18n::Locale::ZhCn),
            "等待确认"
        );
        assert_eq!(
            SessionStatus::AwaitingUser.label(crate::i18n::Locale::En),
            "Needs you"
        );
    }


    // ---- status, badges and the transcript ----

    fn with_status(id: &str, status: SessionStatus) -> TestSession {
        TestSession {
            status,
            ..TestSession::new(id)
        }
    }

    fn one_project(sessions: Vec<TestSession>) -> SessionStore<TestSession> {
        SessionStore::new(
            vec![project("p1", sessions, Some("s1"))],
            ProjectId::new("p1"),
        )
    }

    #[test]
    fn a_turn_that_ends_out_of_sight_leaves_the_session_unread() {
        let mut shell = HarnessShell::new(one_project(vec![
            TestSession::new("s1"),
            with_status("s2", SessionStatus::Running),
        ]));
        let s2 = SessionId::new("s2");
        let badge = |shell: &HarnessShell<TestSession>| {
            let session = shell.store().session(&SessionId::new("s2")).cloned().unwrap();
            shell.badge_of(&session)
        };
        assert_eq!(badge(&shell), SessionBadge::Running);

        // Still running: an update is not an ending.
        shell.update_session_content(&s2, with_status("s2", SessionStatus::Running));
        assert_eq!(badge(&shell), SessionBadge::Running);

        // It finishes while s1 is on screen.
        shell.update_session_content(&s2, TestSession::new("s2"));
        assert_eq!(badge(&shell), SessionBadge::Unread);
        assert_eq!(shell.attention_count(), 0, "finished is not waiting for the user");

        // Looking at it clears the mark, and it stays cleared.
        shell.switch_session(&s2);
        assert_eq!(badge(&shell), SessionBadge::None);
        shell.switch_session(&SessionId::new("s1"));
        assert_eq!(badge(&shell), SessionBadge::None);
    }

    #[test]
    fn a_turn_that_ends_on_screen_is_not_unread() {
        let shell = HarnessShell::new(one_project(vec![with_status(
            "s1",
            SessionStatus::Running,
        )]));
        let s1 = SessionId::new("s1");
        shell.update_session_content(&s1, TestSession::new("s1"));
        let session = shell.store().session(&s1).cloned().unwrap();
        assert_eq!(shell.badge_of(&session), SessionBadge::None, "the user watched it end");
    }

    #[test]
    fn a_turn_that_ends_in_an_error_or_a_question_says_so_instead() {
        let shell = HarnessShell::new(one_project(vec![
            TestSession::new("s1"),
            with_status("s2", SessionStatus::Running),
            with_status("s3", SessionStatus::Running),
        ]));
        shell.update_session_content(
            &SessionId::new("s2"),
            with_status("s2", SessionStatus::Error),
        );
        shell.update_session_content(
            &SessionId::new("s3"),
            with_status("s3", SessionStatus::AwaitingUser),
        );
        let badge = |id: &str| {
            let session = shell.store().session(&SessionId::new(id)).cloned().unwrap();
            shell.badge_of(&session)
        };
        assert_eq!(badge("s2"), SessionBadge::Error);
        assert_eq!(badge("s3"), SessionBadge::AwaitingUser);
        assert_eq!(shell.attention_count(), 2, "both want the user");
    }

    #[test]
    fn archived_sessions_are_kept_but_not_listed_or_counted() {
        let store = one_project(vec![
            TestSession::new("s1"),
            TestSession {
                archived: true,
                ..with_status("s2", SessionStatus::AwaitingUser)
            },
            with_status("s3", SessionStatus::AwaitingUser),
        ]);
        let groups = resolve_sidebar_groups(&store, None);
        let listed: Vec<&str> = groups[0].1.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(listed, ["s1", "s3"]);
        assert!(store.session(&SessionId::new("s2")).is_some(), "still in the project");

        let shell = HarnessShell::new(store);
        assert_eq!(shell.attention_count(), 1, "only s3: s2 is put away");

        assert!(is_archived(nanobot_boot::container::SessionStatus::Archived));
        assert!(!is_archived(nanobot_boot::container::SessionStatus::Active));
        assert_eq!(
            SessionStatus::from(nanobot_boot::container::SessionStatus::Archived),
            SessionStatus::Idle,
            "archived is a flag, not a status"
        );
    }

    #[test]
    fn the_turn_phase_defaults_from_the_plan_and_the_status() {
        let phase = |status, plan_phase| {
            let session = TestSession {
                status,
                plan_phase,
                ..TestSession::new("s1")
            };
            HarnessShell::new(one_project(vec![session])).turn_phase()
        };
        assert_eq!(phase(SessionStatus::Idle, PlanPhase::None), TurnPhase::Idle);
        assert_eq!(phase(SessionStatus::Running, PlanPhase::Executing), TurnPhase::Generating);
        assert_eq!(phase(SessionStatus::Error, PlanPhase::None), TurnPhase::Error);
        assert_eq!(
            phase(SessionStatus::AwaitingUser, PlanPhase::Executing),
            TurnPhase::AwaitingApproval
        );
        // The plan's own waits are more specific than the status.
        assert_eq!(
            phase(SessionStatus::AwaitingUser, PlanPhase::Clarify),
            TurnPhase::AwaitingClarification
        );
        assert_eq!(
            phase(SessionStatus::Running, PlanPhase::Contingent),
            TurnPhase::AwaitingDecision
        );
    }

    #[test]
    fn switching_sessions_puts_that_sessions_conversation_on_screen() {
        let talk = |id: &str, seqs: std::ops::RangeInclusive<u64>| TestSession {
            conversation: Conversation::from_entries(
                seqs.map(|seq| Entry::user(seq, format!("{id} {seq}"))).collect(),
            ),
            ..TestSession::new(id)
        };
        let mut shell = HarnessShell::new(one_project(vec![talk("s1", 1..=3), talk("s2", 10..=11)]));
        let seqs = |shell: &HarnessShell<TestSession>| -> Vec<u64> {
            shell.transcript().entries().iter().map(|entry| entry.seq).collect()
        };
        assert_eq!(seqs(&shell), [1, 2, 3], "the open session is on screen from the start");
        shell.switch_session(&SessionId::new("s2"));
        assert_eq!(seqs(&shell), [10, 11]);
        shell.switch_session(&SessionId::new("s1"));
        assert_eq!(seqs(&shell), [1, 2, 3]);
    }

    // ---- the conversation stream, drawn ----

    /// A shell in a (headless) window on one session whose conversation the
    /// test can stream into.
    fn streaming_shell(
        cx: &mut gpui::TestAppContext,
    ) -> (
        gpui::Entity<HarnessShell<TestSession>>,
        Rc<RefCell<Conversation>>,
        &mut gpui::VisualTestContext,
    ) {
        cx.update(gpui_component::init);
        let live = Rc::new(RefCell::new(Conversation::from_entries(vec![Entry::user(
            1,
            "cut four holes",
        )])));
        let session = TestSession {
            live: Some(live.clone()),
            status: SessionStatus::Running,
            ..TestSession::new("s1")
        };
        let (shell, cx) = cx.add_window_view(|_, _| HarnessShell::new(one_project(vec![session])));
        (shell, live, cx)
    }

    fn redraw(shell: &gpui::Entity<HarnessShell<TestSession>>, cx: &mut gpui::VisualTestContext) {
        shell.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
    }

    fn paragraphs(count: usize) -> String {
        (0..count)
            .map(|ix| format!("Paragraph {ix}: the bracket keeps **4 mm** of wall here.\n\n"))
            .collect()
    }

    #[gpui::test]
    fn an_answer_streams_into_the_list_and_the_end_stays_in_view(cx: &mut gpui::TestAppContext) {
        let (shell, live, cx) = streaming_shell(cx);
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.stream().rows().len(), 1);
            assert!(shell.stream().is_following());
        });

        // The answer arrives a few hundred bytes at a time.
        let answer = paragraphs(160);
        let mut rows_seen = Vec::new();
        for len in (300..answer.len()).step_by(700) {
            let mut cut = len;
            while !answer.is_char_boundary(cut) {
                cut += 1;
            }
            live.borrow_mut().entries.truncate(1);
            live.borrow_mut()
                .entries
                .push(Entry::assistant_streaming(2, &answer[..cut]));
            redraw(&shell, cx);
            shell.read_with(cx, |shell, _| {
                rows_seen.push(shell.stream().rows().len());
                assert!(shell.stream().is_following(), "at {cut} bytes");
                assert!(shell.stream().at_end(), "at {cut} bytes");
            });
        }
        assert!(
            rows_seen.windows(2).all(|pair| pair[0] <= pair[1]),
            "rows only ever get added: {rows_seen:?}"
        );
        assert!(*rows_seen.last().unwrap() > 4, "a long answer is several rows");

        // It ends: a tool call, a check, the turn's end.
        {
            let mut live = live.borrow_mut();
            live.entries.truncate(1);
            live.entries.push(Entry::assistant(2, answer.as_str()));
            live.entries.push(Entry::tool(
                3,
                ToolCall::new(ToolKind::Edit, "pattern_holes(4)", ToolStatus::Done)
                    .with_output("4 holes\n"),
            ));
            live.entries.push(Entry::check(4, "wall thickness", true));
            live.entries.push(Entry::turn_end(5, "12s"));
        }
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            let rows = shell.stream().rows();
            assert_eq!(rows.len(), rows_seen.last().unwrap() + 3);
            assert!(shell.stream().at_end());
        });
    }

    #[gpui::test]
    fn scrolling_up_holds_the_stream_still_until_the_user_goes_back(cx: &mut gpui::TestAppContext) {
        let (shell, live, cx) = streaming_shell(cx);
        let answer = paragraphs(200);
        live.borrow_mut()
            .entries
            .push(Entry::assistant_streaming(2, &answer[..6000]));
        redraw(&shell, cx);
        // Where the session column is in a default-sized test window.
        let over_stream = gpui::point(gpui::px(500.), gpui::px(300.));
        cx.simulate_event(gpui::ScrollEvent {
            position: over_stream,
            delta: gpui::ScrollDelta::Pixels(gpui::point(gpui::px(0.), gpui::px(400.))),
            ..Default::default()
        });
        redraw(&shell, cx);
        let held = shell.read_with(cx, |shell, _| {
            assert!(!shell.stream().is_following(), "scrolling up stops the following");
            shell.stream().scroll_position()
        });

        // The answer keeps arriving; what the user is reading does not move.
        live.borrow_mut().entries.truncate(1);
        live.borrow_mut()
            .entries
            .push(Entry::assistant_streaming(2, answer.as_str()));
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.stream().scroll_position(), held);
            assert!(!shell.stream().at_end());
        });

        // "Jump to end".
        shell.update(cx, |shell, _| shell.stream().follow());
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            assert!(shell.stream().is_following());
            assert!(shell.stream().at_end());
        });
    }

    #[gpui::test]
    fn every_kind_of_row_draws_folded_and_unfolded(cx: &mut gpui::TestAppContext) {
        let (shell, live, cx) = streaming_shell(cx);
        let output: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        live.borrow_mut().entries = vec![
            Entry::user(1, "cut four holes"),
            Entry::thinking(2, "which face is the top one?", Some(1800)),
            Entry::assistant(3, "On it.\n\n```rhai\nlet n = 4;\n```\n"),
            Entry::tool(4, ToolCall::new(ToolKind::Read, "Body1", ToolStatus::Done)),
            Entry::tool(
                5,
                ToolCall::new(ToolKind::Search, "holes", ToolStatus::Failed).with_output("none"),
            ),
            Entry::tool(
                6,
                ToolCall::new(ToolKind::Execute, "run mesher", ToolStatus::Running)
                    .with_input("{ \"size\": 2 }")
                    .with_output(output),
            )
            .with_anchor("Body1.TopFace"),
            Entry::tool(
                7,
                ToolCall::new(ToolKind::Edit, "pattern_holes(4)", ToolStatus::AwaitingApproval),
            ),
            Entry::tool(
                8,
                ToolCall::new(ToolKind::Delete, "remove sketch", ToolStatus::Rejected),
            ),
            Entry::plan(
                9,
                vec![PlanStep {
                    text: "cut".to_string(),
                    status: StepStatus::InProgress,
                }],
            ),
            Entry {
                kind: EntryKind::Check {
                    name: "wall thickness".to_string(),
                    passed: false,
                    details: Some("2.1 mm at hole 3, minimum 3 mm".to_string()),
                },
                ..Entry::check(10, "", false)
            },
            Entry::error(11, "The mesher stopped", "out of memory"),
            Entry::turn_end(12, "34s"),
        ];
        redraw(&shell, cx);
        let keys: Vec<stream::RowKey> =
            shell.read_with(cx, |shell, _| shell.stream().rows().iter().map(|r| r.key).collect());
        assert_eq!(keys.len(), 11, "the read and the search share a row");

        // Unfold everything, then fold it again: nothing panics, nothing is lost.
        for round in 0..2 {
            shell.update(cx, |shell, _| {
                for key in &keys {
                    shell.stream_mut().toggle(*key);
                }
            });
            redraw(&shell, cx);
            shell.read_with(cx, |shell, _| {
                assert_eq!(shell.stream().rows().len(), 11);
                assert_eq!(shell.stream().is_expanded(keys[1]), round == 0);
            });
        }
    }

    // ---- the composer, the queue and approvals ----

    type Sent = Rc<RefCell<Vec<(String, String, Delivery)>>>;

    /// A shell on `sessions` that records every message the product is sent.
    fn recording_shell(sessions: Vec<TestSession>) -> (HarnessShell<TestSession>, Sent) {
        let sent: Sent = Rc::new(RefCell::new(Vec::new()));
        let mut shell = HarnessShell::new(one_project(sessions));
        let log = sent.clone();
        shell.set_on_message_sent(move |id, text, delivery| {
            log.borrow_mut()
                .push((id.as_str().to_string(), text.to_string(), delivery));
        });
        (shell, sent)
    }

    fn texts(sent: &Sent) -> Vec<(String, Delivery)> {
        sent.borrow()
            .iter()
            .map(|(_, text, delivery)| (text.clone(), *delivery))
            .collect()
    }

    #[test]
    fn a_message_to_an_idle_session_is_sent() {
        let (mut shell, sent) = recording_shell(vec![TestSession::new("s1")]);
        assert_eq!(shell.submit_message("  cut holes \n", false), Some(Submitted::Sent(Delivery::Send)));
        assert_eq!(texts(&sent), [("cut holes".to_string(), Delivery::Send)], "trimmed");
        assert_eq!(shell.submit_message("   ", false), None, "nothing to send");
        assert_eq!(sent.borrow().len(), 1);
    }

    #[test]
    fn while_the_agent_works_a_message_queues_and_goes_out_when_it_is_done() {
        let live = Rc::new(RefCell::new(Conversation::default()));
        let running = |status| TestSession {
            status,
            live: Some(live.clone()),
            ..TestSession::new("s1")
        };
        let (mut shell, sent) = recording_shell(vec![running(SessionStatus::Running)]);
        assert_eq!(shell.submit_message("then chamfer", false), Some(Submitted::Queued));
        assert_eq!(shell.submit_message("and fillet", false), Some(Submitted::Queued));
        assert!(sent.borrow().is_empty(), "held, not sent");
        assert_eq!(shell.queued_messages().len(), 2);

        shell.drain_queues();
        assert!(sent.borrow().is_empty(), "the turn is still running");

        // Sent at once instead: it goes into the running turn.
        assert_eq!(
            shell.submit_message("stop, wrong face", true),
            Some(Submitted::Sent(Delivery::Steer))
        );
        assert_eq!(texts(&sent), [("stop, wrong face".to_string(), Delivery::Steer)]);

        // The turn ends: one queued message per turn, in order.
        let s1 = SessionId::new("s1");
        shell.update_session_content(&s1, running(SessionStatus::Idle));
        shell.refresh_workspace();
        shell.drain_queues();
        shell.drain_queues();
        assert_eq!(texts(&sent)[1], ("then chamfer".to_string(), Delivery::Send));
        assert_eq!(sent.borrow().len(), 2, "the second waits for the next turn");

        shell.update_session_content(&s1, running(SessionStatus::Running));
        shell.refresh_workspace();
        shell.drain_queues();
        shell.update_session_content(&s1, running(SessionStatus::Idle));
        shell.refresh_workspace();
        shell.drain_queues();
        assert_eq!(texts(&sent)[2], ("and fillet".to_string(), Delivery::Send));
        assert!(shell.queued_messages().is_empty());
    }

    #[test]
    fn a_queued_message_can_be_taken_back_or_sent_now() {
        let (mut shell, sent) = recording_shell(vec![with_status("s1", SessionStatus::Running)]);
        shell.submit_message("first", false);
        shell.submit_message("second", false);
        let ids: Vec<u64> = shell.queued_messages().iter().map(|m| m.id).collect();

        assert_eq!(shell.take_queued(ids[0]).as_deref(), Some("first"), "to edit or drop");
        assert!(shell.send_queued_now(ids[1]));
        assert_eq!(texts(&sent), [("second".to_string(), Delivery::Steer)]);
        assert!(shell.queued_messages().is_empty());
        assert!(!shell.send_queued_now(ids[1]), "it is gone");
    }

    /// A session stopped on a tool call that waits for approval.
    fn waiting_session() -> TestSession {
        TestSession {
            status: SessionStatus::AwaitingUser,
            conversation: Conversation::from_entries(vec![
                Entry::user(1, "move the holes"),
                Entry::tool(
                    2,
                    ToolCall::new(ToolKind::Edit, "move_pattern(1.5)", ToolStatus::AwaitingApproval),
                ),
            ]),
            ..TestSession::new("s1")
        }
    }

    type Decisions = Rc<RefCell<Vec<(u64, ApprovalDecision)>>>;

    fn approving_shell() -> (HarnessShell<TestSession>, Sent, Decisions) {
        let (mut shell, sent) = recording_shell(vec![waiting_session()]);
        let decisions: Decisions = Rc::new(RefCell::new(Vec::new()));
        let log = decisions.clone();
        shell.set_on_approval(move |_, seq, decision| log.borrow_mut().push((seq, decision)));
        (shell, sent, decisions)
    }

    #[test]
    fn an_approval_is_answered_with_one_of_three_decisions() {
        let (mut shell, _sent, decisions) = approving_shell();
        let approval = shell.pending_approval().expect("the edit is waiting");
        assert_eq!((approval.seq, approval.title.as_str()), (2, "move_pattern(1.5)"));
        assert_eq!(shell.turn_phase(), TurnPhase::AwaitingApproval);

        assert!(shell.decide_approval(ApprovalDecision::AllowOnce));
        assert!(shell.decide_approval(ApprovalDecision::AllowAlways(ApprovalScope::Session)));
        assert!(shell.decide_approval(ApprovalDecision::Reject { feedback: None }));
        assert_eq!(
            *decisions.borrow(),
            [
                (2, ApprovalDecision::AllowOnce),
                (2, ApprovalDecision::AllowAlways(ApprovalScope::Session)),
                (2, ApprovalDecision::Reject { feedback: None }),
            ]
        );

        let (mut idle, _) = recording_shell(vec![TestSession::new("s1")]);
        assert!(!idle.decide_approval(ApprovalDecision::AllowOnce), "nothing is waiting");
    }

    #[test]
    fn typing_while_an_approval_waits_refuses_it_with_what_was_typed() {
        let (mut shell, sent, decisions) = approving_shell();
        assert_eq!(
            shell.submit_message("no, move them 3 mm instead", false),
            Some(Submitted::Refused)
        );
        assert!(sent.borrow().is_empty(), "it is an answer, not a new message");
        assert_eq!(
            *decisions.borrow(),
            [(
                2,
                ApprovalDecision::Reject {
                    feedback: Some("no, move them 3 mm instead".to_string())
                }
            )]
        );
        assert!(shell.queued_messages().is_empty(), "and nothing was queued");
    }

    #[test]
    fn a_queued_message_never_approves_a_waiting_call() {
        // Queued while the turn ran; the turn then stopped on an approval.
        let (mut shell, sent) = recording_shell(vec![with_status("s1", SessionStatus::Running)]);
        let decisions: Decisions = Rc::new(RefCell::new(Vec::new()));
        let log = decisions.clone();
        shell.set_on_approval(move |_, seq, decision| log.borrow_mut().push((seq, decision)));
        shell.submit_message("then chamfer", false);

        let s1 = SessionId::new("s1");
        shell.update_session_content(&s1, waiting_session());
        shell.refresh_workspace();
        shell.drain_queues();
        assert!(sent.borrow().is_empty(), "the queue holds while the user is asked");
        assert!(decisions.borrow().is_empty(), "and decides nothing");
        assert_eq!(shell.queued_messages().len(), 1);
    }

    #[test]
    fn stopping_asks_the_product_only_while_a_turn_is_in_progress() {
        let stopped = Rc::new(RefCell::new(0));
        let count = stopped.clone();
        let mut shell = HarnessShell::new(one_project(vec![with_status(
            "s1",
            SessionStatus::Running,
        )]));
        shell.set_on_stop(move |_| *count.borrow_mut() += 1);
        assert!(shell.stop_turn());
        assert_eq!(*stopped.borrow(), 1);

        let mut idle = HarnessShell::new(one_project(vec![TestSession::new("s1")]));
        let count = stopped.clone();
        idle.set_on_stop(move |_| *count.borrow_mut() += 1);
        assert!(!idle.stop_turn(), "nothing to stop");
        assert_eq!(*stopped.borrow(), 1);
    }

    #[test]
    fn the_permission_dial_is_remembered_and_reported() {
        use std::sync::{Arc, Mutex};

        let settings = Arc::new(Mutex::new(nanobot_boot::Settings::in_memory()));
        let seen = Rc::new(RefCell::new(Vec::new()));
        let log = seen.clone();
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_settings(Some(Arc::clone(&settings)));
        shell.set_on_permission_mode(move |mode| log.borrow_mut().push(mode));
        assert_eq!(shell.permission_mode(), PermissionMode::Ask, "asking is the default");

        shell.cycle_permission_mode();
        shell.cycle_permission_mode();
        assert_eq!(shell.permission_mode(), PermissionMode::PlanOnly);
        assert_eq!(*seen.borrow(), [PermissionMode::AcceptLowRisk, PermissionMode::PlanOnly]);
        shell.set_permission_mode(PermissionMode::PlanOnly);
        assert_eq!(seen.borrow().len(), 2, "no change, nothing to report");

        let mut next = HarnessShell::new(two_project_store());
        next.set_settings(Some(settings));
        assert_eq!(next.permission_mode(), PermissionMode::PlanOnly, "restored at launch");
    }

    // ---- commands, candidates and rewinding ----

    #[test]
    fn the_palette_lists_what_can_run_now() {
        let mut shell = HarnessShell::new(one_project(vec![
            TestSession::new("s1"),
            TestSession::new("s2"),
            TestSession {
                archived: true,
                ..TestSession::new("s3")
            },
        ]));
        let ids = |shell: &HarnessShell<TestSession>| -> Vec<String> {
            shell.commands().into_iter().map(|c| c.id).collect()
        };
        let listed = ids(&shell);
        assert!(listed.contains(&palette::id::TOGGLE_SIDEBAR.to_string()));
        assert!(listed.contains(&"shell.switch_session:s2".to_string()));
        assert!(!listed.contains(&"shell.switch_session:s1".to_string()), "already there");
        assert!(!listed.contains(&"shell.switch_session:s3".to_string()), "archived");
        assert!(!listed.contains(&palette::id::NEW_SESSION.to_string()), "no way to make one");
        assert!(!listed.contains(&palette::id::STOP_TURN.to_string()), "nothing running");

        shell.set_on_new_session(|| {});
        shell.add_command(Command::new("acme.export", "Export STEP"), || {});
        let listed = ids(&shell);
        assert_eq!(listed[0], palette::id::NEW_SESSION);
        assert!(listed.contains(&"acme.export".to_string()));
    }

    #[test]
    fn commands_run() {
        let ran = Rc::new(RefCell::new(Vec::new()));
        let mut shell = HarnessShell::new(one_project(vec![
            TestSession::new("s1"),
            TestSession::new("s2"),
            TestSession::new("s3"),
        ]));
        let log = ran.clone();
        shell.add_command(Command::new("acme.export", "Export STEP"), move || {
            log.borrow_mut().push("export");
        });
        let log = ran.clone();
        shell.set_on_new_session(move || log.borrow_mut().push("new"));

        assert!(shell.run_command("acme.export", 1600.));
        assert!(shell.run_command(palette::id::NEW_SESSION, 1600.));
        assert_eq!(*ran.borrow(), ["export", "new"]);
        assert!(!shell.run_command("no.such", 1600.));

        assert!(shell.run_command(palette::id::TOGGLE_SIDEBAR, 1600.));
        assert!(!shell.sidebar_shown(1600.));

        // Round the sessions and back, and straight to one.
        let on_screen = |shell: &HarnessShell<TestSession>| shell.active_session_id().unwrap().to_string();
        assert!(shell.run_command(palette::id::NEXT_SESSION, 1600.));
        assert_eq!(on_screen(&shell), "s2");
        assert!(shell.run_command(palette::id::PREVIOUS_SESSION, 1600.));
        assert!(shell.run_command(palette::id::PREVIOUS_SESSION, 1600.));
        assert_eq!(on_screen(&shell), "s3", "it wraps");
        assert!(shell.run_command("shell.switch_session:s1", 1600.));
        assert_eq!(on_screen(&shell), "s1");

        assert_eq!(shell.permission_mode(), PermissionMode::Ask);
        shell.run_command(palette::id::CYCLE_PERMISSION_MODE, 1600.);
        assert_eq!(shell.permission_mode(), PermissionMode::AcceptLowRisk);
        assert!(!shell.run_command(palette::id::STOP_TURN, 1600.), "nothing to stop");
    }

    /// Candidates a product might offer.
    fn candidates(sigil: char, query: &str) -> Vec<Candidate> {
        let all: &[(&str, &str)] = match sigil {
            '@' => &[("feature:body1", "Body1"), ("feature:body2", "Body2"), ("face:top", "TopFace")],
            _ => &[("clear", "clear"), ("compact", "compact")],
        };
        all.iter()
            .filter(|(_, label)| label.to_lowercase().starts_with(&query.to_lowercase()))
            .map(|(id, label)| Candidate::new(*id, *label))
            .collect()
    }

    #[test]
    fn candidates_follow_what_is_typed_at_the_caret() {
        let mut shell = HarnessShell::new(one_project(vec![TestSession::new("s1")]));
        shell.update_completion("cut @Bo", 7);
        assert!(shell.completion().is_none(), "no source, no list");

        shell.set_completions(candidates);
        shell.update_completion("cut @Bo", 7);
        let labels = |shell: &HarnessShell<TestSession>| -> Vec<String> {
            shell
                .completion()
                .map(|open| open.candidates.iter().map(|c| c.label.clone()).collect())
                .unwrap_or_default()
        };
        assert_eq!(labels(&shell), ["Body1", "Body2"]);

        // The selection moves, and survives another letter of the same mention.
        assert!(shell.step_completion(1));
        shell.update_completion("cut @Bod", 8);
        let open = shell.completion().unwrap();
        assert_eq!(open.selection.index(open.candidates.len()), Some(1));

        // Accepting hands back what to replace and what with.
        let (trigger, candidate) = shell.accept_completion().expect("one is selected");
        assert_eq!(trigger.range, 4..8);
        assert_eq!((candidate.id.as_str(), candidate.label.as_str()), ("feature:body2", "Body2"));
        assert!(shell.completion().is_none(), "the list closes");

        // Nothing matches, or the caret left the mention: no list.
        shell.update_completion("cut @Zz", 7);
        assert!(shell.completion().is_none());
        shell.update_completion("cut @Bo then", 12);
        assert!(shell.completion().is_none());
        assert!(!shell.step_completion(1));

        // A command at the start of the message.
        shell.update_completion("/c", 2);
        assert_eq!(labels(&shell), ["clear", "compact"]);
        assert!(shell.close_completion());
    }

    type Rewinds = Rc<RefCell<Vec<(u64, RewindMode, String)>>>;

    fn rewinding_shell(session: TestSession) -> (HarnessShell<TestSession>, Sent, Rewinds) {
        let (mut shell, sent) = recording_shell(vec![session]);
        let rewinds: Rewinds = Rc::new(RefCell::new(Vec::new()));
        let log = rewinds.clone();
        shell.set_on_rewind(move |_, seq, mode, text| {
            log.borrow_mut().push((seq, mode, text.to_string()));
        });
        (shell, sent, rewinds)
    }

    fn talked() -> TestSession {
        TestSession {
            conversation: Conversation::from_entries(vec![
                Entry::user(1, "cut four holes"),
                Entry::assistant(2, "Done."),
                Entry::user(3, "now chamfer them"),
                Entry::assistant(4, "Done."),
            ]),
            ..TestSession::new("s1")
        }
    }

    #[test]
    fn rewinding_goes_back_to_a_user_message_with_new_words() {
        let (mut shell, sent, rewinds) = rewinding_shell(talked());
        assert_eq!(shell.begin_rewind(2), None, "an answer is not somewhere to go back to");
        assert_eq!(shell.begin_rewind(99), None);
        assert_eq!(shell.rewinding(), None);

        assert_eq!(shell.begin_rewind(1).as_deref(), Some("cut four holes"));
        assert_eq!(shell.rewinding(), Some(1));
        // The product cannot restore its state: that way back is not offered.
        assert_eq!(shell.rewind_modes(), [RewindMode::Conversation, RewindMode::Fork]);
        assert!(!shell.rewind(RewindMode::ConversationAndState, "cut six holes"));
        assert!(!shell.rewind(RewindMode::Conversation, "   "), "it needs words");
        assert_eq!(shell.rewinding(), Some(1), "still going back");

        // Submitting while going back is the rewind, not a new message.
        assert_eq!(shell.submit_message("cut six holes", false), Some(Submitted::Rewound));
        assert!(sent.borrow().is_empty());
        assert_eq!(
            *rewinds.borrow(),
            [(1, RewindMode::Conversation, "cut six holes".to_string())]
        );
        assert_eq!(shell.rewinding(), None);

        // A fork leaves this session alone; cancelling does nothing at all.
        shell.begin_rewind(3);
        assert!(shell.rewind(RewindMode::Fork, "now fillet them"));
        assert_eq!(rewinds.borrow()[1], (3, RewindMode::Fork, "now fillet them".to_string()));
        shell.begin_rewind(3);
        assert!(shell.cancel_rewind());
        assert!(!shell.rewind(RewindMode::Conversation, "x"), "nothing is being rewound");
        assert_eq!(rewinds.borrow().len(), 2);
    }

    #[test]
    fn without_a_way_to_rewind_messages_cannot_be_gone_back_to() {
        let (mut shell, _) = recording_shell(vec![talked()]);
        assert_eq!(shell.begin_rewind(1), None);
        assert_eq!(shell.rewinding(), None);
    }

    // ---- the keyboard, in a window ----

    /// A shell with a composer in a (headless) window, the keyboard in the
    /// composer, and the shell's bindings installed.
    fn typing_shell(
        cx: &mut gpui::TestAppContext,
    ) -> (gpui::Entity<HarnessShell<TestSession>>, &mut gpui::VisualTestContext) {
        cx.update(gpui_component::init);
        cx.update(crate::init);
        let (shell, cx) = cx.add_window_view(|window, cx| {
            let mut shell = HarnessShell::with_composer(
                one_project(vec![TestSession::new("s1"), TestSession::new("s2")]),
                Locale::En,
                window,
                cx,
            );
            shell.set_completions(candidates);
            shell
        });
        redraw(&shell, cx);
        cx.simulate_keystrokes("ctrl-l");
        redraw(&shell, cx);
        (shell, cx)
    }

    fn composed(shell: &gpui::Entity<HarnessShell<TestSession>>, cx: &mut gpui::VisualTestContext) -> String {
        shell.read_with(cx, |shell, cx| shell.composer_text(cx))
    }

    #[gpui::test]
    fn a_mention_is_picked_with_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        cx.simulate_input("cut @Bo");
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            let open = shell.completion().expect("the list opened as the mention was typed");
            assert_eq!(open.candidates.len(), 2);
        });

        // Down moves in the list, not the caret; Enter takes the row.
        cx.simulate_keystrokes("down");
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            let open = shell.completion().unwrap();
            assert_eq!(open.selection.index(2), Some(1));
        });
        cx.simulate_keystrokes("enter");
        redraw(&shell, cx);
        assert_eq!(composed(&shell, cx), "cut @Body2");
        shell.read_with(cx, |shell, cx| {
            assert!(shell.completion().is_none());
            let tokens = shell.composer_tokens(cx);
            assert_eq!(tokens, ["feature:body2"], "it is a reference, not just text");
        });

        // Escape closes the list and keeps what was typed.
        cx.simulate_input(" and @To");
        redraw(&shell, cx);
        assert!(shell.read_with(cx, |shell, _| shell.completion().is_some()));
        cx.simulate_keystrokes("escape");
        redraw(&shell, cx);
        assert!(shell.read_with(cx, |shell, _| shell.completion().is_none()));
        assert_eq!(composed(&shell, cx), "cut @Body2 and @To");
    }

    #[gpui::test]
    fn the_palette_opens_filters_and_runs_from_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        cx.simulate_keystrokes("ctrl-k");
        redraw(&shell, cx);
        assert!(shell.read_with(cx, |shell, _| shell.palette_is_open()));

        // Typing goes to the palette, not the composer.
        cx.simulate_input("sess");
        redraw(&shell, cx);
        assert_eq!(composed(&shell, cx), "");
        let offered = shell.read_with(cx, |shell, _| shell.palette_matches());
        assert!(offered.len() >= 3 && offered.iter().all(|title| title.to_lowercase().contains("sess")), "{offered:?}");

        // Escape closes it without running anything.
        cx.simulate_keystrokes("escape");
        redraw(&shell, cx);
        assert!(!shell.read_with(cx, |shell, _| shell.palette_is_open()));
        assert_eq!(shell.read_with(cx, |shell, _| shell.active_session_id().unwrap().to_string()), "s1");

        // Open, narrow down to the other session, move, and run.
        cx.simulate_keystrokes("ctrl-k");
        redraw(&shell, cx);
        cx.simulate_input("go to");
        redraw(&shell, cx);
        cx.simulate_keystrokes("down up enter");
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            assert!(!shell.palette_is_open());
            assert_eq!(shell.active_session_id().unwrap().as_str(), "s2");
        });
    }

    // ---- the sidebar ----

    /// The sidebar's rows as short words: "P:p1", "S:s1", "E:2".
    fn sidebar_brief(shell: &HarnessShell<TestSession>) -> Vec<String> {
        shell
            .sidebar_rows()
            .iter()
            .map(|row| match row {
                SidebarRow::Project { id, .. } => format!("P:{id}"),
                SidebarRow::Group { label } => format!("G:{label}"),
                SidebarRow::Session { id, .. } => format!("S:{id}"),
                SidebarRow::Earlier { count, .. } => format!("E:{count}"),
            })
            .collect()
    }

    #[test]
    fn the_sidebar_starts_with_the_open_project_unfolded() {
        let mut shell = HarnessShell::new(two_project_store());
        assert_eq!(sidebar_brief(&shell), ["P:p1", "S:s1", "S:s2", "P:p2"]);

        shell.toggle_project(&ProjectId::new("p2"));
        assert_eq!(sidebar_brief(&shell), ["P:p1", "S:s1", "S:s2", "P:p2", "S:s3"]);
        shell.toggle_project(&ProjectId::new("p1"));
        assert_eq!(sidebar_brief(&shell), ["P:p1", "P:p2", "S:s3"]);
    }

    #[test]
    fn a_session_of_another_project_opens_its_project_first() {
        let mut shell = HarnessShell::new(two_project_store());
        shell.toggle_project(&ProjectId::new("p2"));
        assert!(shell.open_session(&ProjectId::new("p2"), &SessionId::new("s3")));
        assert_eq!(shell.store().open_project_id(), &ProjectId::new("p2"));
        assert_eq!(shell.active_session_id().unwrap().as_str(), "s3");

        // Back, to a session that is not the one the project remembers.
        assert!(shell.open_session(&ProjectId::new("p1"), &SessionId::new("s2")));
        assert_eq!(shell.active_session_id().unwrap().as_str(), "s2");
        assert!(!shell.open_session(&ProjectId::new("p1"), &SessionId::new("nope")));
        assert!(!shell.open_session(&ProjectId::new("p1"), &SessionId::new("s2")), "already there");
    }

    #[test]
    fn the_keyboard_moves_through_the_sidebar_without_changing_what_is_on_screen() {
        let mut shell = HarnessShell::new(two_project_store());
        let on_screen = |shell: &HarnessShell<TestSession>| shell.active_session_id().unwrap().to_string();
        assert_eq!(shell.sidebar_selection(), None);

        shell.step_sidebar(1); // P:p1
        shell.step_sidebar(1); // S:s1
        shell.step_sidebar(1); // S:s2
        assert_eq!(shell.sidebar_selection(), Some(2));
        assert_eq!(shell.selected_session(), Some(SessionId::new("s2")));
        assert_eq!(on_screen(&shell), "s1", "looking is not opening");

        assert!(shell.activate_sidebar_selection());
        assert_eq!(on_screen(&shell), "s2");

        // On a heading, Enter and the arrows fold and unfold.
        shell.step_sidebar(1); // P:p2
        assert_eq!(shell.selected_session(), None);
        shell.set_sidebar_selection_expanded(true);
        assert_eq!(sidebar_brief(&shell).last().unwrap(), "S:s3");
        shell.set_sidebar_selection_expanded(true);
        assert_eq!(sidebar_brief(&shell).len(), 5, "already unfolded: nothing to do");
        shell.set_sidebar_selection_expanded(false);
        assert_eq!(sidebar_brief(&shell).last().unwrap(), "P:p2");
        assert!(shell.activate_sidebar_selection());
        assert_eq!(sidebar_brief(&shell).last().unwrap(), "S:s3");
    }

    #[test]
    fn the_filter_narrows_the_sidebar() {
        let mut shell = HarnessShell::new(two_project_store());
        shell.step_sidebar(1);
        shell.set_sidebar_filter("session s3");
        assert_eq!(sidebar_brief(&shell), ["P:p2", "S:s3"], "its project unfolds for it");
        assert_eq!(shell.sidebar_selection(), None, "the rows under the selection changed");
        shell.set_sidebar_filter("");
        assert_eq!(sidebar_brief(&shell), ["P:p1", "S:s1", "S:s2", "P:p2"]);
    }

    #[test]
    fn the_attention_count_takes_the_keyboard_to_who_is_waiting() {
        let mut shell = HarnessShell::new(one_project(vec![
            TestSession::new("s1"),
            TestSession::new("s2"),
            with_status("s3", SessionStatus::AwaitingUser),
        ]));
        shell.select_first_waiting();
        assert_eq!(shell.selected_session(), Some(SessionId::new("s3")));
        // It sorted to the top of its project.
        assert_eq!(sidebar_brief(&shell), ["P:p1", "S:s3", "S:s1", "S:s2"]);
    }

    #[test]
    fn renaming_and_archiving_ask_the_product() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut shell = HarnessShell::new(two_project_store());
        let s2 = SessionId::new("s2");
        assert!(!shell.rename_session(&s2, "New name"), "no way to rename yet");
        assert!(!shell.archive_session(&s2));

        let log = asked.clone();
        shell.set_on_rename(move |id, title| log.borrow_mut().push(format!("rename {id} {title}")));
        let log = asked.clone();
        shell.set_on_archive(move |id| log.borrow_mut().push(format!("archive {id}")));
        assert!(shell.rename_session(&s2, "  Bracket holes "));
        assert!(!shell.rename_session(&s2, "   "), "a title needs words");
        assert!(shell.archive_session(&s2));
        assert_eq!(*asked.borrow(), ["rename s2 Bracket holes", "archive s2"]);
    }

    #[gpui::test]
    fn the_sidebar_is_driven_from_the_keyboard(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        // From the composer to the sidebar; the first row is selected.
        cx.simulate_keystrokes("ctrl-alt-;");
        redraw(&shell, cx);
        assert_eq!(shell.read_with(cx, |shell, _| shell.sidebar_selection()), Some(0));

        // Down to the second session and open it.
        cx.simulate_keystrokes("down down enter");
        redraw(&shell, cx);
        shell.read_with(cx, |shell, _| {
            assert_eq!(shell.active_session_id().unwrap().as_str(), "s2");
        });
        // Arrows in the sidebar did not type into the composer.
        assert_eq!(composed(&shell, cx), "");

        // Up to the project heading; left folds it away.
        cx.simulate_keystrokes("up up left");
        redraw(&shell, cx);
        assert_eq!(shell.read_with(cx, |shell, _| sidebar_brief(shell)), ["P:p1"]);
        cx.simulate_keystrokes("right");
        redraw(&shell, cx);
        assert_eq!(shell.read_with(cx, |shell, _| sidebar_brief(shell)).len(), 3);
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

    // ---- layout ----

    #[test]
    fn the_badge_counts_other_sessions_waiting_for_the_user() {
        let waiting = |id: &str| TestSession {
            status: SessionStatus::AwaitingUser,
            ..TestSession::new(id)
        };
        let store = SessionStore::new(
            vec![project(
                "p1",
                vec![
                    TestSession::new("s1"),
                    waiting("s2"),
                    TestSession {
                        status: SessionStatus::Running,
                        ..TestSession::new("s3")
                    },
                    waiting("s4"),
                ],
                Some("s1"),
            )],
            ProjectId::new("p1"),
        );
        let mut shell = HarnessShell::new(store);
        assert_eq!(shell.attention_count(), 2, "s2 and s4; running is not waiting");

        // The session on screen is not counted: the user is looking at it.
        shell.switch_session(&SessionId::new("s2"));
        assert_eq!(shell.attention_count(), 1);
    }

    #[test]
    fn hiding_the_sidebar_is_remembered_across_launches() {
        use std::sync::{Arc, Mutex};

        let settings = Arc::new(Mutex::new(nanobot_boot::Settings::in_memory()));
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_settings(Some(Arc::clone(&settings)));
        assert!(shell.sidebar_shown(1600.), "shown by default");

        shell.toggle_sidebar(1600.);
        assert!(!shell.sidebar_shown(1600.));
        assert_eq!(
            settings.lock().unwrap().get(layout::SIDEBAR_OPEN_KEY),
            Some(&serde_json::Value::Bool(false)),
            "written through at once"
        );

        // The next launch reads it back.
        let mut next = HarnessShell::new(two_project_store());
        next.set_settings(Some(Arc::clone(&settings)));
        assert!(!next.sidebar_shown(1600.));
        assert!(!next.layout().sidebar_open);
    }

    #[test]
    fn a_narrow_window_hides_the_sidebar_without_saving_that() {
        use std::sync::{Arc, Mutex};

        let settings = Arc::new(Mutex::new(nanobot_boot::Settings::in_memory()));
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_settings(Some(Arc::clone(&settings)));

        assert!(!shell.sidebar_shown(1100.), "folded away by the width");
        shell.toggle_sidebar(1100.);
        assert!(shell.sidebar_shown(1100.), "opened by hand for now");
        assert!(shell.layout().sidebar_open, "the preference never changed");
        assert_eq!(
            settings.lock().unwrap().get(layout::SIDEBAR_OPEN_KEY),
            None,
            "and nothing was written"
        );
    }

    #[test]
    fn the_viewport_is_the_main_area() {
        let shell = HarnessShell::new(two_project_store());
        let regions = LayoutState::new(shell.layout()).regions(1920., false);
        let (column, viewport) = (regions.column.unwrap(), regions.viewport.unwrap());
        assert_eq!(column, layout::COLUMN_DEFAULT, "the session column keeps its width");
        assert!(viewport > column * 2., "the viewport takes the rest: {viewport}");
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
    fn a_panel_shows_only_with_the_capability_it_requires() {
        use std::cell::Cell;

        let mut host = PanelHost::<TestSession>::default();
        host.register(product_panel("free", "Free", PanelArea::Right, 1));
        host.register(product_panel("pro", "Pro", PanelArea::Right, 2).with_requires("pro"));
        let shown = |host: &PanelHost<TestSession>| -> Vec<String> {
            host.panels_in(PanelArea::Right)
                .into_iter()
                .map(|p| p.id.clone())
                .collect()
        };
        assert_eq!(shown(&host), ["free", "pro"], "nothing injected: everything shows");

        let owned = Rc::new(Cell::new(false));
        let answer = owned.clone();
        host.set_entitlements(Rc::new(move |capability| capability == "pro" && answer.get()));
        assert_eq!(shown(&host), ["free"]);
        assert_eq!(host.visible_panels().len(), 1);
        assert_eq!(host.len(), 2, "the panel stays registered");

        // An activation while the app runs: asked again on the next frame.
        owned.set(true);
        assert_eq!(shown(&host), ["free", "pro"]);
        owned.set(false);
        assert_eq!(shown(&host), ["free"]);
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
    fn sharing_settings_adds_no_panel_and_adopts_what_is_stored() {
        use std::sync::{Arc, Mutex};

        let mut shell = HarnessShell::new(two_project_store());
        let bottom = |shell: &mut HarnessShell<TestSession>| -> Vec<String> {
            shell
                .panel_host()
                .panels_in(PanelArea::Bottom)
                .iter()
                .map(|panel| panel.id.clone())
                .collect()
        };
        assert_eq!(bottom(&mut shell), ["harness.diagnostics"]);

        // Settings live in their dialog, not in the dock.
        let settings = Arc::new(Mutex::new(nanobot_boot::Settings::in_memory()));
        shell.set_settings(Some(settings));
        assert_eq!(bottom(&mut shell), ["harness.diagnostics"]);
        assert!(shell.settings().is_some());

        // A stored theme name becomes the shell's tracked theme; the dialog
        // highlights it on the next frame.
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
    fn the_locale_switches_and_follows_the_settings_store() {
        use std::sync::{Arc, Mutex};

        let mut shell = HarnessShell::new(two_project_store());
        assert_eq!(shell.locale(), crate::i18n::Locale::ZhCn, "the default");

        shell.set_locale(crate::i18n::Locale::En);
        assert_eq!(shell.locale(), crate::i18n::Locale::En);

        // Sharing a store adopts its `ui.locale`, like it adopts the theme.
        let mut settings = nanobot_boot::Settings::in_memory();
        settings
            .set(crate::theme::LOCALE_KEY, serde_json::json!("en"))
            .unwrap();
        shell.set_settings(Some(Arc::new(Mutex::new(settings))));
        assert_eq!(shell.locale(), crate::i18n::Locale::En);
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

    // ---- motion ----

    #[test]
    fn reducing_motion_is_remembered() {
        let mut shell = HarnessShell::new(two_project_store());
        assert!(!shell.reduce_motion(), "motion is on until someone turns it down");

        let settings = std::sync::Arc::new(std::sync::Mutex::new(nanobot_boot::Settings::in_memory()));
        shell.set_settings(Some(settings.clone()));
        let reduce = |shell: &HarnessShell<TestSession>| {
            shell
                .commands()
                .iter()
                .find(|command| command.id == crate::palette::id::TOGGLE_MOTION)
                .map(|command| command.title.clone())
                .unwrap()
        };
        let before = reduce(&shell);
        assert!(shell.run_command(crate::palette::id::TOGGLE_MOTION, 1400.));
        assert!(shell.reduce_motion());
        assert_ne!(reduce(&shell), before, "the command now offers the way back");
        assert_eq!(
            settings.lock().unwrap().get(crate::REDUCE_MOTION_KEY),
            Some(&serde_json::json!(true))
        );

        // A shell that starts on those settings starts still.
        let mut next = HarnessShell::new(two_project_store());
        next.set_settings(Some(settings));
        assert!(next.reduce_motion());
    }

    #[gpui::test]
    fn every_mark_draws_moving_and_still(cx: &mut gpui::TestAppContext) {
        let (shell, live, cx) = streaming_shell(cx);
        *live.borrow_mut() = Conversation::from_entries(vec![
            Entry::user(1, "cut four holes"),
            Entry::tool(2, ToolCall::new(ToolKind::Read, "bracket.step", ToolStatus::Running)),
            Entry::tool(
                3,
                ToolCall::new(ToolKind::Execute, "cargo test", ToolStatus::AwaitingApproval)
                    .with_output("…"),
            ),
            Entry::tool(
                4,
                ToolCall::new(ToolKind::Execute, "cargo build", ToolStatus::Running)
                    .with_output("Compiling"),
            ),
            Entry::assistant_streaming(5, "Cutting the first"),
            Entry::thinking(6, "where does the fourth go", None),
        ]);
        redraw(&shell, cx);
        // Time passes: the marks take their next frames.
        cx.executor().advance_clock(std::time::Duration::from_millis(500));
        redraw(&shell, cx);

        shell.update(cx, |shell, _| shell.set_reduce_motion(true));
        redraw(&shell, cx);
        cx.executor().advance_clock(std::time::Duration::from_millis(500));
        redraw(&shell, cx);
    }

    // ---- the session menu ----

    #[test]
    fn the_session_menu_lists_what_the_product_can_do() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut shell = HarnessShell::new(two_project_store());
        let s2 = SessionId::new("s2");
        assert!(shell.session_actions().is_empty());
        assert!(!shell.open_session_menu(s2.clone(), "session s2", (10., 10.)), "nothing to list");

        for (name, set) in [("fork", 0), ("archive", 1), ("delete", 2)] {
            let log = asked.clone();
            let hook = move |id: &SessionId| log.borrow_mut().push(format!("{name} {id}"));
            match set {
                0 => shell.set_on_fork(hook),
                1 => shell.set_on_archive(hook),
                _ => shell.set_on_delete(hook),
            }
        }
        assert_eq!(
            shell.session_actions(),
            [SessionAction::Fork, SessionAction::Archive, SessionAction::Delete]
        );

        assert!(shell.open_session_menu(s2.clone(), "session s2", (10., 10.)));
        assert_eq!(shell.choose_session_action(SessionAction::Fork), Chosen::Done);
        assert!(!shell.session_menu_is_open());
        assert_eq!(shell.choose_session_action(SessionAction::Fork), Chosen::Nothing, "no menu");

        // Renaming is not offered, so choosing it does nothing.
        shell.open_session_menu(s2.clone(), "session s2", (10., 10.));
        assert_eq!(shell.choose_session_action(SessionAction::Rename), Chosen::Nothing);
        shell.set_on_rename(|_, _| {});
        shell.open_session_menu(s2.clone(), "session s2", (10., 10.));
        assert_eq!(
            shell.choose_session_action(SessionAction::Rename),
            Chosen::Rename { session: s2.clone(), title: "session s2".into() }
        );
        assert_eq!(*asked.borrow(), ["fork s2"]);
    }

    #[test]
    fn deleting_a_session_asks_twice() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut shell = HarnessShell::new(two_project_store());
        let log = asked.clone();
        shell.set_on_delete(move |id| log.borrow_mut().push(id.to_string()));
        let s2 = SessionId::new("s2");

        shell.open_session_menu(s2.clone(), "session s2", (10., 10.));
        assert_eq!(shell.choose_session_action(SessionAction::Delete), Chosen::ConfirmDelete);
        assert!(shell.session_menu_is_open(), "the menu stays to be asked again");
        assert!(asked.borrow().is_empty());
        // Closing it instead forgets the first answer.
        assert!(shell.close_session_menu());
        shell.open_session_menu(s2.clone(), "session s2", (10., 10.));
        assert_eq!(shell.choose_session_action(SessionAction::Delete), Chosen::ConfirmDelete);
        assert_eq!(shell.choose_session_action(SessionAction::Delete), Chosen::Done);
        assert_eq!(*asked.borrow(), ["s2"]);
        assert!(!shell.session_menu_is_open());
    }

    #[test]
    fn the_session_menu_stays_inside_the_window() {
        use crate::session_menu::menu_origin;
        assert_eq!(menu_origin((100., 100.), 4, (1200., 800.)), (100., 100.));
        let (left, top) = menu_origin((1190., 790.), 4, (1200., 800.));
        assert!(left + 200. <= 1200. && top + 4. * 28. <= 800.);
        assert_eq!(menu_origin((-5., -5.), 4, (1200., 800.)), (4., 4.));
    }

    #[gpui::test]
    fn the_right_button_opens_the_menu_and_escape_closes_it(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        shell.update(cx, |shell, _| shell.set_on_archive(|_| {}));
        redraw(&shell, cx);
        shell.update(cx, |shell, _| {
            assert!(shell.open_session_menu(SessionId::new("s2"), "session s2", (40., 120.)));
        });
        redraw(&shell, cx);
        cx.simulate_keystrokes("escape");
        redraw(&shell, cx);
        assert!(!shell.read_with(cx, |shell, _| shell.session_menu_is_open()));
    }

    // ---- retrying ----

    #[gpui::test]
    fn an_error_that_could_pass_next_time_offers_a_retry(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let live = Rc::new(RefCell::new(Conversation::default()));
        let session = |status| TestSession {
            live: Some(live.clone()),
            status,
            ..TestSession::new("s1")
        };
        let failed = session(SessionStatus::Error);
        let (shell, cx) = cx.add_window_view(|_, _| HarnessShell::new(one_project(vec![failed])));
        let set = |entries: Vec<Entry>| *live.borrow_mut() = Conversation::from_entries(entries);
        let can = |cx: &mut gpui::VisualTestContext| shell.read_with(cx, |shell, _| shell.can_retry(2));

        set(vec![Entry::user(1, "mesh it"), Entry::error(2, "Timed out", "no answer").retryable()]);
        redraw(&shell, cx);
        assert!(!can(cx), "the product gave no way to retry");

        let asked = Rc::new(RefCell::new(Vec::new()));
        let log = asked.clone();
        shell.update(cx, |shell, _| {
            shell.set_on_retry(move |id, seq| log.borrow_mut().push(format!("{id} {seq}")));
        });
        assert!(can(cx));
        assert!(shell.update(cx, |shell, _| shell.retry(2)));
        assert_eq!(*asked.borrow(), ["s1 2"]);

        // Not an error that trying again would help.
        set(vec![Entry::user(1, "mesh it"), Entry::error(2, "No licence", "ask sales")]);
        redraw(&shell, cx);
        assert!(!can(cx));
        // Not once the conversation has moved on.
        set(vec![
            Entry::user(1, "mesh it"),
            Entry::error(2, "Timed out", "no answer").retryable(),
            Entry::user(3, "never mind"),
        ]);
        redraw(&shell, cx);
        assert!(!can(cx));
        assert!(!shell.update(cx, |shell, _| shell.retry(2)));
        assert_eq!(asked.borrow().len(), 1);
    }

    #[test]
    fn only_an_error_can_be_marked_retryable() {
        assert_eq!(Entry::user(1, "q").retryable(), Entry::user(1, "q"));
        assert_ne!(Entry::error(2, "t", "m").retryable(), Entry::error(2, "t", "m"));
    }

    // ---- pointing at objects, and the stream's keys ----

    #[test]
    fn a_chip_asks_the_product_to_show_its_object() {
        let asked = Rc::new(RefCell::new(Vec::new()));
        let mut shell = HarnessShell::new(two_project_store());
        assert!(!shell.show_anchor("Body1.TopFace"), "the product cannot show anything yet");
        let log = asked.clone();
        shell.set_on_anchor(move |id, anchor| log.borrow_mut().push(format!("{id} {anchor}")));
        assert!(shell.show_anchor("Body1.TopFace"));
        assert_eq!(*asked.borrow(), ["s1 Body1.TopFace"]);
        // Where only one of the two fits, the viewport is brought up.
        assert_eq!(shell.layout.stacked(), crate::layout::Stacked::Viewport);
    }

    #[gpui::test]
    fn tool_output_unfolds_from_the_keyboard_while_typing(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        let live = Rc::new(RefCell::new(Conversation::from_entries(vec![
            Entry::user(1, "q"),
            Entry::tool(2, ToolCall::new(ToolKind::Edit, "edit", ToolStatus::Done).with_output("x")),
        ])));
        shell.update(cx, |shell, _| {
            shell.stream_mut().set_entries(&live.borrow().entries);
        });
        let open = |cx: &mut gpui::VisualTestContext| {
            shell.read_with(cx, |shell, _| {
                let stream = shell.stream();
                stream.rows().iter().filter(|row| stream.is_expanded(row.key)).count()
            })
        };
        assert_eq!(open(cx), 0);
        cx.simulate_keystrokes("ctrl-o");
        assert_eq!(open(cx), 1, "ctrl-o reaches the shell from the composer");
    }

    #[gpui::test]
    fn a_viewport_alone_in_the_window_keeps_the_composer(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        // Narrow enough that only one of the two fits, and it is the viewport.
        cx.simulate_resize(gpui::size(gpui::px(700.), gpui::px(600.)));
        shell.update(cx, |shell, _| shell.layout.set_stacked(crate::layout::Stacked::Viewport));
        redraw(&shell, cx);
        cx.simulate_keystrokes("ctrl-l");
        cx.simulate_input("add a rib");
        assert_eq!(composed(&shell, cx), "add a rib", "typing still reaches the session");
    }

    // ---- the settings dialog ----

    #[test]
    fn the_settings_dialog_opens_from_the_palette_and_closes() {
        let mut shell = HarnessShell::new(two_project_store());
        assert!(!shell.settings_is_open());
        assert!(shell.commands().iter().any(|c| c.id == crate::palette::id::OPEN_SETTINGS));
        assert!(shell.run_command(crate::palette::id::OPEN_SETTINGS, 1400.));
        assert!(shell.settings_is_open());
        assert!(shell.close_settings());
        assert!(!shell.close_settings(), "it was closed already");

        // One thing at a time is over the window.
        shell.set_on_archive(|_| {});
        shell.open_session_menu(SessionId::new("s2"), "session s2", (10., 10.));
        shell.open_settings();
        assert!(!shell.session_menu_is_open());
    }

    #[gpui::test]
    fn the_settings_dialog_is_drawn_and_escape_closes_it(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        let settings = std::sync::Arc::new(std::sync::Mutex::new(nanobot_boot::Settings::in_memory()));
        shell.update(cx, |shell, _| shell.set_settings(Some(settings)));
        redraw(&shell, cx);
        cx.simulate_keystrokes("ctrl-,");
        redraw(&shell, cx);
        assert!(shell.read_with(cx, |shell, _| shell.settings_is_open()));
        cx.simulate_keystrokes("escape");
        redraw(&shell, cx);
        assert!(!shell.read_with(cx, |shell, _| shell.settings_is_open()));
    }

    // ---- model profiles ----

    fn profile(id: &str, name: &str) -> nanobot_boot::ModelProfile {
        nanobot_boot::ModelProfile {
            id: id.to_string(),
            ..nanobot_boot::ModelProfile::new(name, "https://api.example.com/v1", "wire")
        }
    }

    #[test]
    fn models_are_kept_in_the_settings_and_their_keys_are_not() {
        let settings = std::sync::Arc::new(std::sync::Mutex::new(nanobot_boot::Settings::in_memory()));
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_settings(Some(settings.clone()));
        assert!(shell.session_model().is_none());

        shell.save_model(profile("a", "Fast"), Some("sk-secret"));
        shell.save_model(profile("b", "Deep"), None);
        assert_eq!(shell.session_model().map(|p| p.id.as_str()), Some("a"), "the first is the default");
        assert!(shell.set_default_model("b"));
        assert!(!shell.set_default_model("nope"));

        // Another shell on the same store sees the same list.
        let mut other = HarnessShell::new(two_project_store());
        other.set_settings(Some(settings.clone()));
        assert_eq!(other.models().profiles.len(), 2);
        assert_eq!(other.session_model().map(|p| p.id.as_str()), Some("b"));
        {
            let store = settings.lock().unwrap();
            let listed = store.get(nanobot_boot::models::MODELS_KEY).unwrap().to_string();
            assert!(!listed.contains("sk-secret"));
            assert!(nanobot_boot::models::has_stored_key(&store, "a"));
        }

        // Removing the default leaves the other as the default, and no key.
        assert!(shell.remove_model("b"));
        assert!(shell.remove_model("a"));
        assert!(!shell.remove_model("a"));
        assert!(!nanobot_boot::models::has_stored_key(&settings.lock().unwrap(), "a"));
    }

    #[test]
    fn switching_the_model_tells_the_product_and_leaves_the_default() {
        let mut shell = HarnessShell::new(two_project_store());
        shell.save_model(profile("a", "Fast"), None);
        shell.save_model(profile("b", "Deep"), None);
        assert!(!shell.open_model_menu(None), "the product gave no way to switch");
        assert!(!shell.commands().iter().any(|c| c.id == crate::palette::id::SWITCH_MODEL));

        let chosen = Rc::new(RefCell::new(Vec::new()));
        let seen = chosen.clone();
        shell.set_on_model(move |session, model| {
            seen.borrow_mut().push((session.as_str().to_string(), model.to_string()));
        });
        assert!(shell.run_command(crate::palette::id::SWITCH_MODEL, 1400.));
        assert!(shell.model_menu_is_open());
        assert!(!shell.choose_model("nope"));
        assert!(!shell.model_menu_is_open(), "choosing closes the list");
        assert!(shell.choose_model("b"));
        assert_eq!(*chosen.borrow(), vec![("s1".to_string(), "b".to_string())]);
        assert_eq!(shell.models().default_profile().map(|p| p.id.as_str()), Some("a"));
    }

    #[test]
    fn with_no_model_yet_the_list_opens_the_settings_instead() {
        let mut shell = HarnessShell::new(two_project_store());
        shell.set_on_model(|_, _| {});
        assert!(shell.open_model_menu(Some((10., 700.))));
        assert!(!shell.model_menu_is_open());
        assert!(shell.settings_is_open());
    }

    #[gpui::test]
    fn a_model_is_added_through_the_form_in_the_settings_dialog(cx: &mut gpui::TestAppContext) {
        let (shell, cx) = typing_shell(cx);
        shell.update(cx, |shell, _| {
            shell.set_on_model(|_, _| {});
            shell.save_model(profile("a", "Fast"), None);
            shell.open_settings();
        });
        redraw(&shell, cx);
        shell.update_in(cx, |shell, window, cx| {
            shell.open_model_form(None, window, cx);
            assert!(!shell.submit_model_form(cx), "an empty form is not a model");
            shell.open_model_form(Some("a"), window, cx);
        });
        redraw(&shell, cx);
        // Saved unchanged: still one model, same id.
        assert!(shell.update(cx, |shell, cx| shell.submit_model_form(cx)));
        shell.update(cx, |shell, _| {
            assert_eq!(shell.models().profiles.len(), 1);
            assert_eq!(shell.models().get("a").unwrap().name, "Fast");
            shell.open_model_menu(Some((10., 700.)));
        });
        redraw(&shell, cx);
        cx.simulate_keystrokes("escape");
        assert!(!shell.read_with(cx, |shell, _| shell.model_menu_is_open()));
    }
}
