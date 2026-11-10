//! What a conversation is made of, and how the shell keeps up with it
//! (docs/ui-redesign-design.md §5.2, §11).
//!
//! A conversation is a list of [`Entry`] values, each anchored to the event
//! sequence that produced it. Entries
//! say what they are — a user message, an assistant answer in markdown, a
//! thinking span, a tool call with its kind, status and output — because the
//! shell draws each differently: most tool calls as one line, a few as a card.
//!
//! The product hands over its conversation whole ([`Conversation`]); it is a
//! projection of an event log, and re-projecting is the product's simplest
//! path. [`Transcript`] holds the one on screen and says whether a new one
//! differs; what changed row by row is worked out where the rows are
//! ([`crate::stream`]).
//!
//! [`TurnPhase`] is the other half: where the turn stands, stated by the
//! product rather than guessed from the last entry. What the send button does
//! and what the activity bar shows both read it.

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

    /// Takes a new projection. True when it differs from the one on screen.
    pub fn replace(&mut self, conversation: Conversation) -> bool {
        let changed = self.entries != conversation.entries;
        self.entries = conversation.entries;
        changed
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

/// The mark a session carries in the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
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
            assert!(phase.is_busy());
        }
        assert!(!TurnPhase::Idle.is_busy() && !TurnPhase::Error.is_busy());
    }

    #[test]
    fn a_badge_says_what_a_session_needs() {
        use SessionBadge as B;
        assert_eq!(B::of(SessionStatus::AwaitingUser, true), B::AwaitingUser);
        assert_eq!(B::of(SessionStatus::Error, false), B::Error);
        assert_eq!(B::of(SessionStatus::Running, true), B::Running);
        assert_eq!(B::of(SessionStatus::Idle, true), B::Unread);
        assert_eq!(B::of(SessionStatus::Idle, false), B::None);
        assert!(B::AwaitingUser.needs_attention() && B::Error.needs_attention());
        assert!(!B::Running.needs_attention() && !B::Unread.needs_attention());
    }

    #[test]
    fn a_transcript_says_whether_the_projection_changed() {
        let mut transcript = Transcript::default();
        let talk = || Conversation::from_entries(vec![Entry::user(1, "hi")]);
        assert!(transcript.replace(talk()));
        assert!(!transcript.replace(talk()), "the same projection again");
        assert!(transcript.replace(Conversation::default()));
        assert!(transcript.entries().is_empty());
    }
}
