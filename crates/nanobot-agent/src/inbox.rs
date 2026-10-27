//! Inbox: dual-queue message buffer for the agent (design-harness 10.5).
//!
//! The inbox maintains two queues:
//! - `next_turn`: messages that start a new turn
//! - `next_step`: messages that inject into the current step (steering)
//!
//! One turn claims one next-turn message plus all pending next-step
//! messages. This ensures steering messages are processed before the
//! next turn begins.

use std::collections::VecDeque;
use std::sync::Mutex;

/// Dual-queue message buffer for agent input.
#[derive(Default)]
pub struct Inbox {
    next_turn: Mutex<VecDeque<String>>,
    next_step: Mutex<VecDeque<String>>,
}

impl Inbox {
    /// Create an empty inbox.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a message that will start a new turn.
    pub fn push_turn(&self, message: impl Into<String>) {
        self.next_turn.lock().unwrap().push_back(message.into());
    }

    /// Push a message that will inject into the current step (steering).
    pub fn push_step(&self, message: impl Into<String>) {
        self.next_step.lock().unwrap().push_back(message.into());
    }

    /// Pop the next turn message, if any.
    pub fn pop_turn(&self) -> Option<String> {
        self.next_turn.lock().unwrap().pop_front()
    }

    /// Pop the next step message, if any.
    pub fn pop_step(&self) -> Option<String> {
        self.next_step.lock().unwrap().pop_front()
    }


    /// Check whether there are pending step messages.
    pub fn has_step(&self) -> bool {
        !self.next_step.lock().unwrap().is_empty()
    }


}
