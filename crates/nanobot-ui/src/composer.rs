//! What the composer and the activity bar decide before anything is drawn
//! (docs/ui-redesign-design.md §6, §7): where a submitted message goes, the
//! queue of messages held for later, the permission dial, and what the user
//! can answer a pending tool call with.
//!
//! All of it is plain data, tested without a window.

use crate::transcript::{Entry, EntryKind, ToolKind, ToolStatus, TurnPhase};

/// How a message reaches the agent.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Delivery {
    /// A message for a session that is waiting for one: a new turn, or the
    /// answer to a question the turn stopped on.
    Send,
    /// A message for a turn in progress, delivered at its next tool-call
    /// boundary: the user is changing its course.
    Steer,
}

/// What submitting the composer does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Submit {
    Deliver(Delivery),
    /// Hold the message until the turn ends.
    Queue,
}

/// Where a message submitted during `phase` goes. `immediate` is the user
/// asking for it to go now rather than wait its turn.
///
/// While the agent works, a message waits in the queue unless the user sends
/// it at once. While the turn is stopped on the user, what they type *is*
/// their answer, so it goes straight through.
pub fn submit(phase: TurnPhase, immediate: bool) -> Submit {
    match (phase, immediate) {
        (TurnPhase::Generating, false) => Submit::Queue,
        (TurnPhase::Generating, true) => Submit::Deliver(Delivery::Steer),
        _ => Submit::Deliver(Delivery::Send),
    }
}

/// A message held until the turn ends.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Queued {
    /// Stable for as long as the message is queued; what the UI addresses it by.
    pub id: u64,
    pub text: String,
}

/// The messages a session holds for later, oldest first.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MessageQueue {
    items: Vec<Queued>,
    next_id: u64,
    /// One message went out and the session has not been seen to leave idle
    /// since: nothing more is sent until it has, or a slow product would get
    /// the whole queue at once.
    awaiting_turn: bool,
}

impl MessageQueue {
    pub fn items(&self) -> &[Queued] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Adds a message at the end; returns its id.
    pub fn push(&mut self, text: impl Into<String>) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Queued {
            id,
            text: text.into(),
        });
        id
    }

    /// Takes a message out: to drop it, to put it back in the composer for
    /// editing, or to send it at once.
    pub fn take(&mut self, id: u64) -> Option<String> {
        let ix = self.items.iter().position(|item| item.id == id)?;
        Some(self.items.remove(ix).text)
    }

    /// Whether a message went out and the turn it starts has not been seen yet.
    pub fn is_awaiting_turn(&self) -> bool {
        self.awaiting_turn
    }

    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The session is seen in `phase`: the next message to send now, if any.
    ///
    /// Only an idle session takes one. A turn that stopped on an approval, a
    /// question or a plan is not idle — a queued message must never stand in
    /// for the user's answer — and neither is one that ended in an error,
    /// where the user should look before more is sent.
    pub fn next_for(&mut self, phase: TurnPhase) -> Option<String> {
        if phase != TurnPhase::Idle {
            self.awaiting_turn = false;
            return None;
        }
        if self.awaiting_turn || self.items.is_empty() {
            return None;
        }
        self.awaiting_turn = true;
        Some(self.items.remove(0).text)
    }
}

/// How much the agent may do without asking: the dial below the composer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum PermissionMode {
    /// Ask before every action that changes the product.
    #[default]
    Ask,
    /// Reads and reversible actions go through; the rest asks.
    AcceptLowRisk,
    /// Plan, but do not act.
    PlanOnly,
    /// Never ask.
    FullAccess,
}

impl PermissionMode {
    pub const ALL: [Self; 4] = [
        Self::Ask,
        Self::AcceptLowRisk,
        Self::PlanOnly,
        Self::FullAccess,
    ];

    /// The next mode round the dial.
    pub fn next(self) -> Self {
        let ix = Self::ALL.iter().position(|mode| *mode == self).unwrap_or(0);
        Self::ALL[(ix + 1) % Self::ALL.len()]
    }

    /// Whether the mode lets the agent change things unasked: shown in the
    /// warning colour, so it is never on without the user seeing it.
    pub fn is_unguarded(self) -> bool {
        self == Self::FullAccess
    }

    /// The word the mode is stored as in settings.
    pub fn code(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::AcceptLowRisk => "accept_low_risk",
            Self::PlanOnly => "plan_only",
            Self::FullAccess => "full_access",
        }
    }

    /// The mode a stored word names; anything unknown is the safe default.
    pub fn from_code(code: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|mode| mode.code() == code)
            .unwrap_or_default()
    }

    pub fn label(self, locale: crate::i18n::Locale) -> &'static str {
        crate::i18n::t(
            locale,
            match self {
                Self::Ask => "permission.ask",
                Self::AcceptLowRisk => "permission.accept_low_risk",
                Self::PlanOnly => "permission.plan_only",
                Self::FullAccess => "permission.full_access",
            },
        )
    }
}

/// How far "always allow" reaches.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum ApprovalScope {
    /// Calls like this one, for the rest of this session.
    Session,
    /// Every call of this kind, from now on.
    Kind,
}

/// The user's answer to a tool call that waits for one.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways(ApprovalScope),
    /// Refused, with what the user wants done instead when they said.
    Reject { feedback: Option<String> },
}

/// A tool call waiting for the user.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Approval {
    /// The event of the call: what the answer is addressed to.
    pub seq: u64,
    pub kind: ToolKind,
    pub title: String,
    /// The call's arguments, when it has any worth showing.
    pub detail: Option<String>,
}

/// The tool call the conversation is stopped on: the earliest one waiting.
pub fn pending_approval(entries: &[Entry]) -> Option<Approval> {
    entries.iter().find_map(|entry| match &entry.kind {
        EntryKind::Tool(call) if call.status == ToolStatus::AwaitingApproval => Some(Approval {
            seq: entry.seq,
            kind: call.kind,
            title: call.title.clone(),
            detail: call.input.clone(),
        }),
        _ => None,
    })
}

/// How full the context window is, as the status line colours it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ContextLevel {
    Normal,
    /// Past 70%: worth knowing.
    Warning,
    /// Past 90%: about to matter.
    Critical,
}

impl ContextLevel {
    /// `used` is the share of the window in use, 0 to 1.
    pub fn of(used: f32) -> Self {
        if used >= 0.9 {
            Self::Critical
        } else if used >= 0.7 {
            Self::Warning
        } else {
            Self::Normal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::ToolCall;

    #[test]
    fn a_message_waits_its_turn_unless_sent_at_once() {
        use Delivery::{Send, Steer};
        assert_eq!(submit(TurnPhase::Idle, false), Submit::Deliver(Send));
        assert_eq!(submit(TurnPhase::Idle, true), Submit::Deliver(Send));
        assert_eq!(submit(TurnPhase::Error, false), Submit::Deliver(Send));
        // The agent is working.
        assert_eq!(submit(TurnPhase::Generating, false), Submit::Queue);
        assert_eq!(submit(TurnPhase::Generating, true), Submit::Deliver(Steer));
        // The turn is stopped on the user: what they type is the answer.
        for phase in [
            TurnPhase::AwaitingApproval,
            TurnPhase::AwaitingClarification,
            TurnPhase::AwaitingDecision,
        ] {
            assert_eq!(submit(phase, false), Submit::Deliver(Send), "{phase:?}");
            assert_eq!(submit(phase, true), Submit::Deliver(Send), "{phase:?}");
        }
    }

    #[test]
    fn the_queue_sends_one_message_per_turn_in_order() {
        let mut queue = MessageQueue::default();
        queue.push("first");
        queue.push("second");
        assert_eq!(queue.next_for(TurnPhase::Generating), None, "still working");

        assert_eq!(queue.next_for(TurnPhase::Idle).as_deref(), Some("first"));
        // The product has not flipped to running yet: nothing more goes out.
        assert_eq!(queue.next_for(TurnPhase::Idle), None);
        assert_eq!(queue.next_for(TurnPhase::Idle), None);
        // The turn runs and ends.
        assert_eq!(queue.next_for(TurnPhase::Generating), None);
        assert_eq!(queue.next_for(TurnPhase::Idle).as_deref(), Some("second"));
        assert!(queue.is_empty());
        assert_eq!(queue.next_for(TurnPhase::Idle), None);
    }

    #[test]
    fn a_queued_message_never_answers_for_the_user() {
        for phase in [
            TurnPhase::AwaitingApproval,
            TurnPhase::AwaitingClarification,
            TurnPhase::AwaitingDecision,
            TurnPhase::Error,
        ] {
            let mut queue = MessageQueue::default();
            queue.push("carry on");
            assert_eq!(queue.next_for(phase), None, "{phase:?}");
            assert_eq!(queue.len(), 1, "it stays queued");
        }
    }

    #[test]
    fn a_queued_message_can_be_taken_back() {
        let mut queue = MessageQueue::default();
        let first = queue.push("first");
        let second = queue.push("second");
        let third = queue.push("third");
        assert_eq!(queue.take(second).as_deref(), Some("second"));
        assert_eq!(queue.take(second), None, "only once");
        let left: Vec<u64> = queue.items().iter().map(|item| item.id).collect();
        assert_eq!(left, [first, third]);
        // Ids are not reused, so a stale click cannot hit another message.
        assert!(queue.push("fourth") > third);
        queue.clear();
        assert!(queue.is_empty());
    }

    #[test]
    fn the_dial_goes_round_and_survives_settings() {
        use PermissionMode::{AcceptLowRisk, Ask, FullAccess, PlanOnly};
        assert_eq!(PermissionMode::default(), Ask, "asking is the default");
        assert_eq!(Ask.next(), AcceptLowRisk);
        assert_eq!(AcceptLowRisk.next(), PlanOnly);
        assert_eq!(PlanOnly.next(), FullAccess);
        assert_eq!(FullAccess.next(), Ask);
        for mode in PermissionMode::ALL {
            assert_eq!(PermissionMode::from_code(mode.code()), mode);
            assert_eq!(mode.is_unguarded(), mode == FullAccess);
        }
        assert_eq!(PermissionMode::from_code("yolo"), Ask, "unknown is the safe one");
    }

    #[test]
    fn the_earliest_waiting_call_is_the_pending_approval() {
        let call = |status| ToolCall::new(ToolKind::Edit, "move_pattern", status);
        let entries = vec![
            Entry::user(1, "go"),
            Entry::tool(2, call(ToolStatus::Done)),
            Entry::tool(
                3,
                call(ToolStatus::AwaitingApproval).with_input("{ \"offset\": 1.5 }"),
            ),
            Entry::tool(4, call(ToolStatus::AwaitingApproval)),
        ];
        let approval = pending_approval(&entries).expect("one is waiting");
        assert_eq!(approval.seq, 3);
        assert_eq!(approval.title, "move_pattern");
        assert_eq!(approval.detail.as_deref(), Some("{ \"offset\": 1.5 }"));
        assert_eq!(pending_approval(&entries[..2]), None);
    }

    #[test]
    fn the_context_gauge_changes_colour_at_70_and_90_percent() {
        assert_eq!(ContextLevel::of(0.0), ContextLevel::Normal);
        assert_eq!(ContextLevel::of(0.69), ContextLevel::Normal);
        assert_eq!(ContextLevel::of(0.7), ContextLevel::Warning);
        assert_eq!(ContextLevel::of(0.89), ContextLevel::Warning);
        assert_eq!(ContextLevel::of(0.9), ContextLevel::Critical);
        assert_eq!(ContextLevel::of(1.2), ContextLevel::Critical);
    }
}
