//! The generic harness session model (design 4.8).
//!
//! The shell renders a **session** as the unit of work: its conversation stream,
//! its plan state, its workspace. What the framework fixes here is the
//! *vocabulary* (ids, lifecycle status, plan phases, conversation blocks) and
//! the *store* arithmetic (one project open at a time, a session index per
//! project). What it does **not** fix is the session's own type: a harness has
//! its own domain surfaces (a CAD session carries a feature tree and checks), so
//! the session is product-defined and reaches the shell through
//! [`SessionView`], exactly the way a product reaches the harness through
//! `harness.toml` and the tool executor.

use std::fmt;

use nanobot_gpu::ViewportSlot;

/// Identity of a project — stable across renames and moves.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct ProjectId(String);

impl ProjectId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ProjectId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

/// Identity of a design session.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for SessionId {
    fn from(id: &str) -> Self {
        Self::new(id)
    }
}

/// Lifecycle status of a session.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SessionStatus {
    /// The session the workspace currently shows.
    Active,
    /// Background session still executing.
    Running,
    /// Waiting for the user's review — the decision bar belongs here.
    AwaitingReview,
    Archived,
}

impl SessionStatus {
    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        match self {
            SessionStatus::Active => crate::i18n::t(locale, "status.active"),
            SessionStatus::Running => crate::i18n::t(locale, "status.running"),
            SessionStatus::AwaitingReview => crate::i18n::t(locale, "status.awaiting_review"),
            SessionStatus::Archived => crate::i18n::t(locale, "status.archived"),
        }
    }
}

/// The container's session status maps 1:1 onto the shell's (the shell owns
/// the UI wording; see [`SessionStatus::label`]).
impl From<nanobot_boot::container::SessionStatus> for SessionStatus {
    fn from(status: nanobot_boot::container::SessionStatus) -> Self {
        match status {
            nanobot_boot::container::SessionStatus::Active => Self::Active,
            nanobot_boot::container::SessionStatus::Running => Self::Running,
            nanobot_boot::container::SessionStatus::AwaitingReview => Self::AwaitingReview,
            nanobot_boot::container::SessionStatus::Archived => Self::Archived,
        }
    }
}

/// One step of a session's plan.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PlanStep {
    pub text: String,
    pub status: StepStatus,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
}

impl StepStatus {
    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        match self {
            StepStatus::Pending => crate::i18n::t(locale, "step.pending"),
            StepStatus::InProgress => crate::i18n::t(locale, "step.in_progress"),
            StepStatus::Done => crate::i18n::t(locale, "step.done"),
        }
    }
}

/// `(done, total)` of a plan — the progress the sidebar row and header show.
pub fn plan_progress(plan: &[PlanStep]) -> (usize, usize) {
    let done = plan.iter().filter(|s| s.status == StepStatus::Done).count();
    (done, plan.len())
}

/// Where the plan sits in its lifecycle, as the UI shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PlanPhase {
    #[default]
    None,
    Proposed,
    Executing,
    Verifying,
    Settled,
    /// Verification failed: the plan awaits a decision.
    Contingent,
    Rejected,
    /// The agent needs more information before proposing.
    Clarify,
}

impl PlanPhase {
    /// Whether this phase is waiting for the user to decide.
    pub fn awaits_decision(self) -> bool {
        matches!(self, PlanPhase::Contingent | PlanPhase::Clarify)
    }

    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        match self {
            PlanPhase::None => crate::i18n::t(locale, "plan.none"),
            PlanPhase::Proposed => crate::i18n::t(locale, "plan.proposed"),
            PlanPhase::Executing => crate::i18n::t(locale, "plan.executing"),
            PlanPhase::Verifying => crate::i18n::t(locale, "plan.verifying"),
            PlanPhase::Settled => crate::i18n::t(locale, "plan.settled"),
            PlanPhase::Contingent => crate::i18n::t(locale, "plan.contingent"),
            PlanPhase::Rejected => crate::i18n::t(locale, "plan.rejected"),
            PlanPhase::Clarify => crate::i18n::t(locale, "plan.clarify"),
        }
    }
}

/// What the user decided about a contingent plan.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PlanDecision {
    /// Keep the work and close the plan.
    Accept,
    /// Roll the plan's work back and close it.
    Reject,
    /// Send the plan back for revision.
    Modify,
}

/// The kind of a conversation block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BlockKind {
    User,
    Assistant,
    Tool,
    Plan,
    Check,
}

impl BlockKind {
    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        match self {
            BlockKind::User => crate::i18n::t(locale, "block.user"),
            BlockKind::Assistant => crate::i18n::t(locale, "block.assistant"),
            BlockKind::Tool => crate::i18n::t(locale, "block.tool"),
            BlockKind::Plan => crate::i18n::t(locale, "block.plan"),
            BlockKind::Check => crate::i18n::t(locale, "block.check"),
        }
    }
}

/// Execution state of a tool block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolState {
    Running,
    Done,
    Failed,
}

impl ToolState {
    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        match self {
            ToolState::Running => crate::i18n::t(locale, "tool.running"),
            ToolState::Done => crate::i18n::t(locale, "tool.done"),
            ToolState::Failed => crate::i18n::t(locale, "tool.failed"),
        }
    }
}

/// One block of the conversation stream, anchored to an event sequence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Block {
    pub kind: BlockKind,
    pub text: String,
    pub event_seq: u64,
    pub tool_state: Option<ToolState>,
    /// Optional spatial anchor (a selection reference) rendered as a chip.
    pub selection_ref: Option<String>,
}

impl Block {
    pub fn new(kind: BlockKind, event_seq: u64, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
            event_seq,
            tool_state: None,
            selection_ref: None,
        }
    }

    pub fn user(event_seq: u64, text: impl Into<String>) -> Self {
        Self::new(BlockKind::User, event_seq, text)
    }

    pub fn assistant(event_seq: u64, text: impl Into<String>) -> Self {
        Self::new(BlockKind::Assistant, event_seq, text)
    }

    pub fn tool(event_seq: u64, text: impl Into<String>, state: ToolState) -> Self {
        Self::new(BlockKind::Tool, event_seq, text).with_tool_state(state)
    }

    pub fn plan(event_seq: u64, text: impl Into<String>) -> Self {
        Self::new(BlockKind::Plan, event_seq, text)
    }

    pub fn check(event_seq: u64, text: impl Into<String>) -> Self {
        Self::new(BlockKind::Check, event_seq, text)
    }

    pub fn with_tool_state(mut self, state: ToolState) -> Self {
        self.tool_state = Some(state);
        self
    }

    pub fn with_selection(mut self, selection_ref: impl Into<String>) -> Self {
        self.selection_ref = Some(selection_ref.into());
        self
    }
}

/// The conversation stream of a session.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Conversation {
    pub blocks: Vec<Block>,
}

impl Conversation {
    pub fn from_blocks(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }

    pub fn last_seq(&self) -> u64 {
        self.blocks.last().map(|b| b.event_seq).unwrap_or(0)
    }

    /// Blocks must anchor to strictly increasing event sequences.
    pub fn anchors_are_monotonic(&self) -> bool {
        self.blocks
            .windows(2)
            .all(|pair| pair[0].event_seq < pair[1].event_seq)
    }
}

/// Fork relationship to a parent session.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BranchMarker {
    pub parent_session: SessionId,
    pub parent_seq: u32,
}

/// The active session's whole workspace — the "one unit" a switch replaces.
///
/// The product implements this over its own session type, so its domain
/// surfaces (a feature tree, properties, checks) stay in the product while the
/// generic surfaces the shell renders are read through this trait.
pub trait WorkspaceView: Clone + 'static {
    fn session_id(&self) -> &SessionId;
    fn title(&self) -> String;
    fn status(&self) -> SessionStatus;
    fn conversation(&self) -> Conversation;
    fn plan(&self) -> Vec<PlanStep>;
    fn plan_phase(&self) -> PlanPhase;
    fn clarify_questions(&self) -> Vec<String>;
    fn progress(&self) -> Option<(u32, u32)>;
    /// Geometry calculation progress. The default keeps a workspace that has no
    /// such concept unchanged.
    fn set_progress(&mut self, _progress: Option<(u32, u32)>) {}
    /// Settlement items shown in the approval bar as per-item accept/reject
    /// chips. `(name, accepted)`. Empty by default: a harness whose plans settle
    /// as a whole has no per-item decision to offer.
    fn settlement_items(&self) -> Vec<(String, bool)> {
        Vec::new()
    }
}

/// A session as the shell sees it. The product implements this over its own
/// session type; the associated types are the product's workspace, viewport and
/// scene.
pub trait SessionView: Clone + 'static {
    /// The renderer's scene wire format.
    type Scene: 'static;
    /// The product's viewport (implements [`ViewportSlot`]).
    type Viewport: ViewportSlot<Scene = Self::Scene>;
    /// The product's workspace projection.
    type Workspace: WorkspaceView;

    fn id(&self) -> &SessionId;
    fn title(&self) -> String;
    fn status(&self) -> SessionStatus;
    fn conversation(&self) -> Conversation;
    fn plan(&self) -> Vec<PlanStep>;
    fn plan_phase(&self) -> PlanPhase;
    fn clarify_questions(&self) -> Vec<String>;
    fn branch_marker(&self) -> Option<BranchMarker>;

    /// Builds the whole workspace for this session, owning a fresh viewport
    /// handle. Called on every activation, so switching back and forth gives
    /// each session its own viewport state.
    fn build_workspace(&self, viewport: <Self::Viewport as ViewportSlot>::Handle) -> Self::Workspace;
}

/// One sidebar group of a product-supplied projection.
#[derive(Clone, Debug)]
pub struct SidebarGroup {
    /// The group's heading, rendered when a projection is active.
    pub label: String,
    /// The sessions in the group, in render order.
    pub sessions: Vec<SessionId>,
}

impl SidebarGroup {
    pub fn new(label: impl Into<String>, sessions: Vec<SessionId>) -> Self {
        Self {
            label: label.into(),
            sessions,
        }
    }
}

/// An alternative sidebar arrangement ("one sidebar, two roots" — the design
/// harness's design-rooted vs session-rooted view of the same project graph).
///
/// The product owns grouping and labels; the framework owns rendering and
/// selection (a click still switches sessions, and sessions the projection
/// never named stay reachable under a trailing group — a projection is a
/// view, not a strainer). `resolve_sidebar_groups` is the contract: one flat
/// group when no projection is set, the product's groups otherwise.
pub trait SidebarProjection<S: SessionView> {
    fn groups(&self, store: &SessionStore<S>) -> Vec<SidebarGroup>;
}

/// The sidebar's groups: the projection's when one is supplied, otherwise one
/// flat group of the open project's sessions in order (the default view).
pub fn resolve_sidebar_groups<S: SessionView>(
    store: &SessionStore<S>,
    projection: Option<&dyn SidebarProjection<S>>,
) -> Vec<(String, Vec<S>)> {
    let sessions: Vec<S> = store.open_project_sessions().to_vec();
    let Some(projection) = projection else {
        return vec![(store.open_project().name.clone(), sessions)];
    };
    let mut groups: Vec<(String, Vec<S>)> = Vec::new();
    let mut listed: Vec<SessionId> = Vec::new();
    for group in projection.groups(store) {
        let members: Vec<S> = group
            .sessions
            .iter()
            .filter_map(|id| store.session(id).cloned())
            .collect();
        if members.is_empty() {
            continue;
        }
        listed.extend(members.iter().map(|s| s.id().clone()));
        groups.push((group.label, members));
    }
    let rest: Vec<S> = sessions
        .into_iter()
        .filter(|s| !listed.contains(s.id()))
        .collect();
    if !rest.is_empty() {
        groups.push((String::from("\u{5176}\u{4ed6}"), rest));
    }
    groups
}

/// One project: the unit that groups sessions.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Project<S> {
    pub id: ProjectId,
    pub name: String,
    pub sessions: Vec<S>,
    /// The session to restore when this project is (re)opened. `None` while the
    /// project has no sessions at all.
    pub session_index: Option<SessionId>,
}

impl<S: SessionView> Project<S> {
    pub fn session(&self, id: &SessionId) -> Option<&S> {
        self.sessions.iter().find(|s| s.id() == id)
    }
}

/// The projects the harness knows about, with exactly one open.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionStore<S> {
    projects: Vec<Project<S>>,
    open: ProjectId,
}

impl<S: SessionView> SessionStore<S> {
    /// `open` must name one of `projects` (the shell constructor relies on it).
    pub fn new(projects: Vec<Project<S>>, open: ProjectId) -> Self {
        assert!(
            projects.iter().any(|p| p.id == open),
            "the open project must be one of the known projects"
        );
        Self { projects, open }
    }

    /// The recent-project list behind the switcher.
    pub fn projects(&self) -> &[Project<S>] {
        &self.projects
    }

    pub fn open_project_id(&self) -> &ProjectId {
        &self.open
    }

    pub fn project(&self, id: &ProjectId) -> Option<&Project<S>> {
        self.projects.iter().find(|p| p.id == *id)
    }

    /// The single open project: its sessions are what the sidebar lists.
    pub fn open_project(&self) -> &Project<S> {
        self.project(&self.open)
            .expect("the open project is always one of the known projects")
    }

    pub fn open_project_sessions(&self) -> &[S] {
        &self.open_project().sessions
    }

    /// A session of the open project — sessions of closed projects are not
    /// addressable (only one project is open).
    pub fn session(&self, id: &SessionId) -> Option<&S> {
        self.open_project().session(id)
    }

    /// Opens `id`. False when `id` is unknown or already open.
    pub fn set_open_project(&mut self, id: &ProjectId) -> bool {
        if self.open == *id || self.project(id).is_none() {
            return false;
        }
        self.open = id.clone();
        true
    }

    /// Updates the open project's session index. False when `id` is not a
    /// session of the open project.
    pub fn set_session_index(&mut self, id: &SessionId) -> bool {
        let open = self.open.clone();
        let Some(project) = self.projects.iter_mut().find(|p| p.id == open) else {
            return false;
        };
        if project.session(id).is_none() {
            return false;
        }
        project.session_index = Some(id.clone());
        true
    }

    /// Replaces a session's content with data loaded from the session's event
    /// log. False when `id` is not a session of the open project.
    pub fn update_session_content(&mut self, id: &SessionId, session: S) -> bool {
        let open = self.open.clone();
        let Some(project) = self.projects.iter_mut().find(|p| p.id == open) else {
            return false;
        };
        let Some(existing) = project.sessions.iter_mut().find(|s| s.id() == id) else {
            return false;
        };
        *existing = session;
        true
    }

    /// Build the store from a boot container's manifest: one project, its
    /// session index loaded through `loader`.
    ///
    /// The bridge a product shell needs on startup: the framework owns the
    /// manifest, the paths and the session index (the container); the product
    /// knows how to turn one [`nanobot_boot::container::SessionEntry`] plus
    /// the project paths into its own session (usually by loading that
    /// session's event log under `paths`). Entry order is the index's; the
    /// index's active session, when present, becomes the project's session
    /// index. A project with an empty index still opens — the shell shows it
    /// empty.
    pub fn from_manifest<M, F>(
        manifest: &M,
        paths: &nanobot_boot::container::ProjectPaths,
        loader: F,
    ) -> Self
    where
        M: nanobot_boot::container::Manifest,
        F: Fn(&nanobot_boot::container::SessionEntry, &nanobot_boot::container::ProjectPaths) -> S,
    {
        let info = manifest.project_info();
        let index = manifest.sessions();
        let sessions: Vec<S> = index
            .entries
            .iter()
            .map(|entry| loader(entry, paths))
            .collect();
        let session_index = index.active.as_ref().and_then(|id| {
            let want = SessionId::new(id.clone());
            sessions
                .iter()
                .find(|session| session.id() == &want)
                .map(|_| want)
        });
        let project_id = ProjectId::new(info.uuid.clone());
        Self::new(
            vec![Project {
                id: project_id.clone(),
                name: info.name.clone(),
                sessions,
                session_index,
            }],
            project_id,
        )
    }
}
