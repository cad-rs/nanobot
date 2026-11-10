//! What a conversation is made of, and how the shell keeps up with it
//! (docs/ui-redesign-design.md §5.2, §11).
//!
//! A conversation is a list of [`Entry`] values, each anchored to the event
//! sequence that produced it. Unlike the flat text blocks they replace, entries
//! say what they are — a user message, an assistant answer in markdown, a
//! thinking span, a tool call with its kind, status and output — because the
//! shell draws each differently: most tool calls as one line, a few as a card.
//!
//! The product still hands over its conversation whole ([`Conversation`]); it
//! is a projection of an event log, and re-projecting is the product's simplest
//! path. What the shell does with it is [`Transcript::reconcile`]: compare the
//! new projection with the one on screen and reduce the difference to
//! [`Change`]s at entry indices — appended here, updated there, removed from
//! there on. A virtualised list applies those in place instead of rebuilding,
//! which is what keeps a streaming answer cheap.
//!
//! [`TurnPhase`] is the other half: where the turn stands, stated by the
//! product rather than guessed from the last entry. What the send button does,
//! what the activity bar shows and which badge a session carries all read it.

use std::ops::Range;

use crate::model::{PlanStep, SessionStatus};

/// What kind of thing a tool call does. The icon and the default density
/// follow the kind, not the tool's name, so a product's tools need no table in
/// the shell. (The kinds are the Agent Client Protocol's.)
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

impl ToolKind {
    /// Whether a call of this kind changes the product's state.
    pub fn mutates(self) -> bool {
        matches!(self, Self::Edit | Self::Delete | Self::Move | Self::Execute)
    }
}

/// Where a tool call stands.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ToolStatus {
    /// Waiting for the user to allow or refuse it.
    AwaitingApproval,
    Running,
    Done,
    Failed,
    /// Stopped before it finished (the turn was interrupted).
    Cancelled,
    /// The user refused it.
    Rejected,
}

impl ToolStatus {
    /// Whether the call is over, one way or another.
    pub fn is_settled(self) -> bool {
        !matches!(self, Self::AwaitingApproval | Self::Running)
    }

    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        crate::i18n::t(
            locale,
            match self {
                Self::AwaitingApproval => "tool.awaiting_approval",
                Self::Running => "tool.running",
                Self::Done => "tool.done",
                Self::Failed => "tool.failed",
                Self::Cancelled => "tool.cancelled",
                Self::Rejected => "tool.rejected",
            },
        )
    }
}

/// One tool call.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ToolCall {
    pub kind: ToolKind,
    /// What the call does, in a line: the name and its telling argument.
    pub title: String,
    pub status: ToolStatus,
    /// The arguments, for the expanded view.
    pub input: Option<String>,
    /// What it produced so far. May grow while the call runs.
    pub output: Option<String>,
    /// What it changed, as a unified diff (`-` and `+` lines; `@@` hunk
    /// headers and file headers are optional).
    pub diff: Option<String>,
}

impl ToolCall {
    pub fn new(kind: ToolKind, title: impl Into<String>, status: ToolStatus) -> Self {
        Self {
            kind,
            title: title.into(),
            status,
            input: None,
            output: None,
            diff: None,
        }
    }

    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = Some(input.into());
        self
    }

    pub fn with_output(mut self, output: impl Into<String>) -> Self {
        self.output = Some(output.into());
        self
    }

    pub fn with_diff(mut self, diff: impl Into<String>) -> Self {
        self.diff = Some(diff.into());
        self
    }

    /// The density rule: a call is drawn as a card only when it needs the
    /// user's answer, changes the product's state, has a diff to show, or is
    /// producing output as
    /// it runs. Everything else — the reads and searches that make up most of
    /// a turn — is one line.
    pub fn is_card(&self) -> bool {
        self.status == ToolStatus::AwaitingApproval
            || self.kind.mutates()
            || self.diff.is_some()
            || (self.status == ToolStatus::Running && self.output.is_some())
    }
}

/// What an entry is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum EntryKind {
    User {
        text: String,
    },
    /// An answer, in markdown. `streaming` while more is still arriving.
    Assistant {
        markdown: String,
        streaming: bool,
    },
    /// The model's reasoning, shown folded.
    Thinking {
        text: String,
        /// How long it took, once it is over.
        duration_ms: Option<u64>,
    },
    Tool(ToolCall),
    /// The plan as it was proposed at this point (a snapshot: the live plan is
    /// the workspace's).
    Plan {
        steps: Vec<PlanStep>,
    },
    Check {
        name: String,
        passed: bool,
        details: Option<String>,
    },
    /// The line a turn ends with: how long it took, what it changed.
    TurnEnd {
        summary: String,
    },
    Error {
        title: String,
        message: String,
        /// Whether trying again could help.
        retryable: bool,
    },
}

/// One entry of a conversation, anchored to an event sequence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The event this entry came from. Strictly increasing along a
    /// conversation; it is what [`Transcript::reconcile`] matches entries by.
    pub seq: u64,
    pub kind: EntryKind,
    /// An object of the product this entry is about (a selection reference),
    /// shown as a chip that points back into the viewport.
    pub anchor: Option<String>,
}

impl Entry {
    pub fn new(seq: u64, kind: EntryKind) -> Self {
        Self {
            seq,
            kind,
            anchor: None,
        }
    }

    pub fn user(seq: u64, text: impl Into<String>) -> Self {
        Self::new(seq, EntryKind::User { text: text.into() })
    }

    /// A finished answer.
    pub fn assistant(seq: u64, markdown: impl Into<String>) -> Self {
        Self::new(
            seq,
            EntryKind::Assistant {
                markdown: markdown.into(),
                streaming: false,
            },
        )
    }

    /// An answer still arriving.
    pub fn assistant_streaming(seq: u64, markdown: impl Into<String>) -> Self {
        Self::new(
            seq,
            EntryKind::Assistant {
                markdown: markdown.into(),
                streaming: true,
            },
        )
    }

    pub fn thinking(seq: u64, text: impl Into<String>, duration_ms: Option<u64>) -> Self {
        Self::new(
            seq,
            EntryKind::Thinking {
                text: text.into(),
                duration_ms,
            },
        )
    }

    pub fn tool(seq: u64, call: ToolCall) -> Self {
        Self::new(seq, EntryKind::Tool(call))
    }

    pub fn plan(seq: u64, steps: Vec<PlanStep>) -> Self {
        Self::new(seq, EntryKind::Plan { steps })
    }

    pub fn check(seq: u64, name: impl Into<String>, passed: bool) -> Self {
        Self::new(
            seq,
            EntryKind::Check {
                name: name.into(),
                passed,
                details: None,
            },
        )
    }

    pub fn turn_end(seq: u64, summary: impl Into<String>) -> Self {
        Self::new(
            seq,
            EntryKind::TurnEnd {
                summary: summary.into(),
            },
        )
    }

    pub fn error(seq: u64, title: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(
            seq,
            EntryKind::Error {
                title: title.into(),
                message: message.into(),
                retryable: false,
            },
        )
    }

    pub fn with_anchor(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// Marks an error entry as one that trying again could help. Nothing
    /// changes on any other kind of entry.
    pub fn retryable(mut self) -> Self {
        if let EntryKind::Error { retryable, .. } = &mut self.kind {
            *retryable = true;
        }
        self
    }
}

/// A session's conversation, as the product projects it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Conversation {
    pub entries: Vec<Entry>,
}

impl Conversation {
    pub fn from_entries(entries: Vec<Entry>) -> Self {
        Self { entries }
    }

    pub fn last_seq(&self) -> u64 {
        self.entries.last().map_or(0, |entry| entry.seq)
    }

    /// Entries must anchor to strictly increasing event sequences.
    pub fn anchors_are_monotonic(&self) -> bool {
        self.entries
            .windows(2)
            .all(|pair| pair[0].seq < pair[1].seq)
    }
}

/// How the entries on screen differ from a new projection, at entry indices.
/// A list applies them in the order given.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    /// The entry at this index is the same event with new content (a
    /// streaming answer grew, a tool call finished).
    Updated(usize),
    /// The entries in this range are gone (a rewind, or history that changed
    /// from here on).
    Removed(Range<usize>),
    /// `count` entries were added starting at index `at`, which is the end of
    /// the list at that point.
    Appended { at: usize, count: usize },
}

/// The conversation on screen: the last projection the shell was given.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Transcript {
    entries: Vec<Entry>,
}

impl Transcript {
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Takes a new projection and returns what changed, in the order a list
    /// should apply it.
    ///
    /// Entries are matched by their event sequence. While the sequences agree
    /// from the start, an entry whose content differs is an update. From the
    /// first index where they disagree, the old entries are removed and the
    /// new ones appended: histories that diverge share only their beginning.
    pub fn reconcile(&mut self, conversation: Conversation) -> Vec<Change> {
        let new = conversation.entries;
        let shared = self
            .entries
            .iter()
            .zip(&new)
            .take_while(|(old, new)| old.seq == new.seq)
            .count();
        let mut changes: Vec<Change> = (0..shared)
            .filter(|&ix| self.entries[ix] != new[ix])
            .map(Change::Updated)
            .collect();
        if self.entries.len() > shared {
            changes.push(Change::Removed(shared..self.entries.len()));
        }
        if new.len() > shared {
            changes.push(Change::Appended {
                at: shared,
                count: new.len() - shared,
            });
        }
        self.entries = new;
        changes
    }
}

/// Where a turn stands. The product states it
/// ([`WorkspaceView::turn_phase`](crate::model::WorkspaceView::turn_phase));
/// the shell does not work it out from the last entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum TurnPhase {
    /// Nothing is running; the session takes a new message.
    #[default]
    Idle,
    /// The agent is working: answering, thinking or running tools.
    Generating,
    /// A tool call is waiting for the user to allow or refuse it.
    AwaitingApproval,
    /// The agent asked a question it needs answered before going on.
    AwaitingClarification,
    /// A plan is waiting for the user to accept, reject or revise it.
    AwaitingDecision,
    /// The turn ended in an error.
    Error,
}

impl TurnPhase {
    /// Whether the agent is in the middle of a turn — working or stopped on a
    /// question only the user can answer.
    pub fn is_busy(self) -> bool {
        !matches!(self, Self::Idle | Self::Error)
    }

    /// Whether the turn is stopped until the user does something.
    pub fn awaits_user(self) -> bool {
        matches!(
            self,
            Self::AwaitingApproval | Self::AwaitingClarification | Self::AwaitingDecision
        )
    }

    /// What the composer's button does now, given whether anything is typed.
    pub fn composer_action(self, has_input: bool) -> ComposerAction {
        match (self, has_input) {
            // Typing while an approval waits answers it (a refusal with the
            // text as feedback), and an answer to a question is a message.
            (Self::Idle | Self::Error, _) => ComposerAction::Send,
            (Self::AwaitingApproval | Self::AwaitingClarification | Self::AwaitingDecision, true) => {
                ComposerAction::Send
            }
            (Self::Generating, true) => ComposerAction::Queue,
            (_, false) => ComposerAction::Stop,
        }
    }

    /// The session status this phase amounts to, as the sidebar counts it.
    pub fn status(self) -> SessionStatus {
        match self {
            Self::Idle => SessionStatus::Idle,
            Self::Generating => SessionStatus::Running,
            Self::Error => SessionStatus::Error,
            _ => SessionStatus::AwaitingUser,
        }
    }

    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        crate::i18n::t(
            locale,
            match self {
                Self::Idle => "turn.idle",
                Self::Generating => "turn.generating",
                Self::AwaitingApproval => "turn.awaiting_approval",
                Self::AwaitingClarification => "turn.awaiting_clarification",
                Self::AwaitingDecision => "turn.awaiting_decision",
                Self::Error => "turn.error",
            },
        )
    }
}

/// What the composer's one button does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ComposerAction {
    /// Send the message now.
    Send,
    /// Hold the message until the turn ends.
    Queue,
    /// Stop the turn.
    Stop,
}

/// The mark a session carries in the sidebar. More urgent ones come first:
/// a session waiting for the user outranks one that merely finished.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum SessionBadge {
    /// Waiting for the user: an approval, a question or a plan to decide.
    AwaitingUser,
    /// The last turn ended in an error.
    Error,
    Running,
    /// Finished while the user was looking at something else.
    Unread,
    /// Nothing to say: read, or never run.
    None,
}

impl SessionBadge {
    /// The badge for a session with `status`; `unread` is the shell's own
    /// knowledge that it finished out of sight.
    pub fn of(status: SessionStatus, unread: bool) -> Self {
        match status {
            SessionStatus::AwaitingUser => Self::AwaitingUser,
            SessionStatus::Error => Self::Error,
            SessionStatus::Running => Self::Running,
            SessionStatus::Idle if unread => Self::Unread,
            SessionStatus::Idle => Self::None,
        }
    }

    /// Whether the user has something to do here — what the toolbar's count
    /// adds up.
    pub fn needs_attention(self) -> bool {
        matches!(self, Self::AwaitingUser | Self::Error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(seq: u64, status: ToolStatus) -> Entry {
        Entry::tool(seq, ToolCall::new(ToolKind::Read, "read Body1", status))
    }

    #[test]
    fn only_some_tool_calls_are_cards() {
        let call = |kind, status| ToolCall::new(kind, "x", status);
        // Reading and searching: a line, whatever the status.
        assert!(!call(ToolKind::Read, ToolStatus::Running).is_card());
        assert!(!call(ToolKind::Search, ToolStatus::Done).is_card());
        assert!(!call(ToolKind::Fetch, ToolStatus::Failed).is_card());
        // Anything that changes the product: a card.
        for kind in [ToolKind::Edit, ToolKind::Delete, ToolKind::Move, ToolKind::Execute] {
            assert!(call(kind, ToolStatus::Done).is_card(), "{kind:?}");
        }
        // Anything waiting for the user: a card, even a read.
        assert!(call(ToolKind::Read, ToolStatus::AwaitingApproval).is_card());
        // Output arriving while it runs: a card; once done, a line again.
        assert!(call(ToolKind::Other, ToolStatus::Running)
            .with_output("…")
            .is_card());
        assert!(!call(ToolKind::Other, ToolStatus::Done)
            .with_output("…")
            .is_card());
    }

    #[test]
    fn anchors_must_strictly_increase() {
        let mut conversation =
            Conversation::from_entries(vec![Entry::user(1, "hi"), Entry::assistant(2, "hello")]);
        assert!(conversation.anchors_are_monotonic());
        assert_eq!(conversation.last_seq(), 2);
        conversation.entries[1].seq = 1;
        assert!(!conversation.anchors_are_monotonic());
        assert_eq!(Conversation::default().last_seq(), 0);
    }

    #[test]
    fn a_first_projection_is_all_appended() {
        let mut transcript = Transcript::default();
        let changes = transcript.reconcile(Conversation::from_entries(vec![
            Entry::user(1, "hi"),
            Entry::assistant(2, "hello"),
        ]));
        assert_eq!(changes, [Change::Appended { at: 0, count: 2 }]);
        assert_eq!(transcript.len(), 2);
        // The same projection again: nothing to do.
        let again = Conversation::from_entries(transcript.entries().to_vec());
        assert!(transcript.reconcile(again).is_empty());
    }

    #[test]
    fn a_streaming_answer_updates_one_entry() {
        let mut transcript = Transcript::default();
        transcript.reconcile(Conversation::from_entries(vec![
            Entry::user(1, "hi"),
            Entry::assistant_streaming(2, "hel"),
        ]));
        let changes = transcript.reconcile(Conversation::from_entries(vec![
            Entry::user(1, "hi"),
            Entry::assistant_streaming(2, "hello, wor"),
        ]));
        assert_eq!(changes, [Change::Updated(1)], "the user message is untouched");

        // It finishes, and a tool call follows in the same projection.
        let changes = transcript.reconcile(Conversation::from_entries(vec![
            Entry::user(1, "hi"),
            Entry::assistant(2, "hello, world"),
            read(3, ToolStatus::Running),
        ]));
        assert_eq!(
            changes,
            [Change::Updated(1), Change::Appended { at: 2, count: 1 }]
        );
    }

    #[test]
    fn a_tool_call_finishing_is_an_update() {
        let mut transcript = Transcript::default();
        transcript.reconcile(Conversation::from_entries(vec![
            read(1, ToolStatus::Running),
            read(2, ToolStatus::Running),
        ]));
        let changes = transcript.reconcile(Conversation::from_entries(vec![
            read(1, ToolStatus::Done),
            read(2, ToolStatus::Running),
        ]));
        assert_eq!(changes, [Change::Updated(0)]);
    }

    #[test]
    fn a_rewind_removes_the_tail() {
        let mut transcript = Transcript::default();
        transcript.reconcile(Conversation::from_entries(
            (1..=5).map(|seq| Entry::user(seq, "m")).collect(),
        ));
        let changes = transcript.reconcile(Conversation::from_entries(
            (1..=2).map(|seq| Entry::user(seq, "m")).collect(),
        ));
        assert_eq!(changes, [Change::Removed(2..5)]);
        assert_eq!(transcript.len(), 2);

        let changes = transcript.reconcile(Conversation::default());
        assert_eq!(changes, [Change::Removed(0..2)]);
        assert!(transcript.is_empty());
    }

    #[test]
    fn histories_that_diverge_keep_only_their_shared_start() {
        let mut transcript = Transcript::default();
        transcript.reconcile(Conversation::from_entries(vec![
            Entry::user(1, "a"),
            Entry::assistant(2, "b"),
            Entry::user(3, "c"),
        ]));
        // After a rewind to 1 the session went on differently: 4, 5.
        let changes = transcript.reconcile(Conversation::from_entries(vec![
            Entry::user(1, "a"),
            Entry::user(4, "d"),
            Entry::assistant(5, "e"),
        ]));
        assert_eq!(
            changes,
            [Change::Removed(1..3), Change::Appended { at: 1, count: 2 }]
        );
        assert_eq!(transcript.entries()[1].seq, 4);
    }

    #[test]
    fn another_sessions_conversation_replaces_everything() {
        let mut transcript = Transcript::default();
        transcript.reconcile(Conversation::from_entries(vec![Entry::user(7, "a")]));
        let changes =
            transcript.reconcile(Conversation::from_entries(vec![Entry::user(1, "b")]));
        assert_eq!(
            changes,
            [Change::Removed(0..1), Change::Appended { at: 0, count: 1 }]
        );
    }

    /// Applying the changes to a mirror of the old list gives the new one:
    /// what a virtualised list does with them.
    #[test]
    fn changes_replay_onto_a_list() {
        let steps: Vec<Vec<Entry>> = vec![
            vec![Entry::user(1, "a")],
            vec![Entry::user(1, "a"), Entry::assistant_streaming(2, "b")],
            vec![
                Entry::user(1, "a"),
                Entry::assistant(2, "bb"),
                read(3, ToolStatus::Running),
            ],
            vec![Entry::user(1, "a")],
            vec![Entry::user(1, "a"), Entry::user(9, "z"), Entry::user(10, "y")],
            vec![],
        ];
        let mut transcript = Transcript::default();
        let mut mirror: Vec<u64> = Vec::new();
        for step in steps {
            for change in transcript.reconcile(Conversation::from_entries(step.clone())) {
                match change {
                    Change::Updated(ix) => assert_eq!(mirror[ix], step[ix].seq),
                    Change::Removed(range) => {
                        mirror.drain(range);
                    }
                    Change::Appended { at, count } => {
                        assert_eq!(at, mirror.len(), "appends land at the end");
                        mirror.extend(step[at..at + count].iter().map(|entry| entry.seq));
                    }
                }
            }
            let expected: Vec<u64> = step.iter().map(|entry| entry.seq).collect();
            assert_eq!(mirror, expected);
        }
    }

    #[test]
    fn the_composer_button_follows_the_turn() {
        use ComposerAction::{Queue, Send, Stop};
        assert_eq!(TurnPhase::Idle.composer_action(true), Send);
        assert_eq!(TurnPhase::Idle.composer_action(false), Send, "nothing to stop");
        assert_eq!(TurnPhase::Error.composer_action(false), Send);
        // Busy with something typed: hold it. Busy with nothing typed: stop.
        assert_eq!(TurnPhase::Generating.composer_action(true), Queue);
        assert_eq!(TurnPhase::Generating.composer_action(false), Stop);
        // Stopped on the user: what they type is their answer.
        for phase in [
            TurnPhase::AwaitingApproval,
            TurnPhase::AwaitingClarification,
            TurnPhase::AwaitingDecision,
        ] {
            assert_eq!(phase.composer_action(true), Send, "{phase:?}");
            assert_eq!(phase.composer_action(false), Stop, "{phase:?}");
            assert!(phase.awaits_user() && phase.is_busy());
        }
        assert!(!TurnPhase::Idle.is_busy() && !TurnPhase::Error.is_busy());
    }

    #[test]
    fn badges_rank_by_urgency() {
        use SessionBadge as B;
        assert_eq!(B::of(SessionStatus::AwaitingUser, true), B::AwaitingUser);
        assert_eq!(B::of(SessionStatus::Error, false), B::Error);
        assert_eq!(B::of(SessionStatus::Running, true), B::Running);
        assert_eq!(B::of(SessionStatus::Idle, true), B::Unread);
        assert_eq!(B::of(SessionStatus::Idle, false), B::None);

        let mut badges = [B::None, B::Unread, B::Running, B::Error, B::AwaitingUser];
        badges.sort();
        assert_eq!(
            badges,
            [B::AwaitingUser, B::Error, B::Running, B::Unread, B::None]
        );
        assert!(B::AwaitingUser.needs_attention() && B::Error.needs_attention());
        assert!(!B::Running.needs_attention() && !B::Unread.needs_attention());
    }

    #[test]
    fn a_turn_phase_amounts_to_a_session_status() {
        assert_eq!(TurnPhase::Idle.status(), SessionStatus::Idle);
        assert_eq!(TurnPhase::Generating.status(), SessionStatus::Running);
        assert_eq!(TurnPhase::AwaitingApproval.status(), SessionStatus::AwaitingUser);
        assert_eq!(TurnPhase::AwaitingDecision.status(), SessionStatus::AwaitingUser);
        assert_eq!(TurnPhase::Error.status(), SessionStatus::Error);
    }
}
