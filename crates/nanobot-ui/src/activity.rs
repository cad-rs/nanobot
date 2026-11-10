//! The activity bar and the composer (docs/ui-redesign-design.md §6, §7): what
//! sits between the conversation and the bottom of the session column.
//!
//! The bar gathers what used to be scattered above the conversation — the
//! plan, a plan awaiting a decision, a question from the agent — and adds the
//! tool call waiting for approval and the messages queued for later. Each part
//! shows only when it has something to say; with nothing, the bar takes no
//! room. It sits against the composer because that is where the user's hands
//! are when any of it needs an answer.
//!
//! The first half of this file is what the shell *does* (submit, stop,
//! answer an approval, turn the dial, drain the queue) and runs without a
//! window; the second half draws it.

use gpui::{
    div, prelude::*, px, AnyElement, Context, Div, Hsla, MouseButton, MouseDownEvent, Stateful,
    Window,
};
use gpui_component::input::Textarea;
use gpui_component::ActiveTheme as _;
use serde_json::Value;

use crate::composer::{
    pending_approval, submit, Approval, ApprovalDecision, ApprovalScope, ContextLevel, Delivery,
    PermissionMode, Submit,
};
use crate::icons::{icon, Icon};
use crate::model::{
    plan_progress, PlanDecision, PlanPhase, SessionId, SessionStatus, SessionView, StepStatus,
    WorkspaceView,
};
use crate::shell::{palette, HarnessShell, Palette};
use crate::transcript::{ComposerAction, TurnPhase};

/// The setting the permission dial is remembered under.
pub const PERMISSION_MODE_KEY: &str = "agent.permission_mode";

/// What became of a submitted message.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Submitted {
    Sent(Delivery),
    Queued,
    /// It answered the tool call that was waiting: a refusal, with the text
    /// as what to do instead.
    Refused,
    /// It replaced the message being rewound to.
    Rewound,
}

impl<S: SessionView> HarnessShell<S> {
    /// The tool call the session on screen is stopped on, if any.
    pub fn pending_approval(&self) -> Option<Approval> {
        pending_approval(self.transcript().entries())
    }

    /// Submits `text` for the session on screen. `immediate` sends it into a
    /// running turn instead of queueing it. `None` when there is nothing to
    /// submit or no session to submit to.
    pub fn submit_message(&mut self, text: &str, immediate: bool) -> Option<Submitted> {
        let text = text.trim();
        let id = self.active_session_id()?.clone();
        if text.is_empty() {
            return None;
        }
        // A message is being rewound to: what is submitted is its new text.
        if self.rewinding.is_some() {
            return self
                .rewind(crate::commands::RewindMode::Conversation, text)
                .then_some(Submitted::Rewound);
        }
        // A tool call is waiting: what the user types instead of pressing a
        // button is a refusal that says what to do instead.
        if let Some(approval) = self.pending_approval() {
            self.answer_approval(
                &id,
                approval.seq,
                ApprovalDecision::Reject {
                    feedback: Some(text.to_string()),
                },
            );
            return Some(Submitted::Refused);
        }
        match submit(self.turn_phase(), immediate) {
            Submit::Queue => {
                self.queues.entry(id).or_default().push(text);
                Some(Submitted::Queued)
            }
            Submit::Deliver(delivery) => {
                self.deliver(&id, text, delivery);
                Some(Submitted::Sent(delivery))
            }
        }
    }

    fn deliver(&self, id: &SessionId, text: &str, delivery: Delivery) {
        if let Some(sender) = self.on_message_sent.clone() {
            (sender.borrow())(id, text, delivery);
        }
    }

    fn answer_approval(&mut self, id: &SessionId, seq: u64, decision: ApprovalDecision) {
        self.always_scope_open = false;
        if let Some(callback) = self.on_approval.clone() {
            (callback.borrow())(id, seq, decision);
        }
    }

    /// Answers the tool call the session on screen is stopped on. False when
    /// none is waiting.
    pub fn decide_approval(&mut self, decision: ApprovalDecision) -> bool {
        let (Some(id), Some(approval)) = (self.active_session_id().cloned(), self.pending_approval())
        else {
            return false;
        };
        self.answer_approval(&id, approval.seq, decision);
        true
    }

    /// Asks the product to stop the turn of the session on screen. False when
    /// no turn is in progress.
    pub fn stop_turn(&mut self) -> bool {
        let Some(id) = self.active_session_id().cloned() else {
            return false;
        };
        if !self.turn_phase().is_busy() {
            return false;
        }
        if let Some(callback) = self.on_stop.clone() {
            (callback.borrow())(&id);
        }
        true
    }

    /// How much the agent may do unasked.
    pub fn permission_mode(&self) -> PermissionMode {
        self.permission_mode
    }

    /// Sets the dial, remembers it and tells the product.
    pub fn set_permission_mode(&mut self, mode: PermissionMode) {
        if self.permission_mode == mode {
            return;
        }
        self.permission_mode = mode;
        if let Some(Ok(mut settings)) = self.settings.as_ref().map(|s| s.lock()) {
            let _ = settings.set(PERMISSION_MODE_KEY, Value::from(mode.code()));
        }
        if let Some(callback) = self.on_permission_mode.clone() {
            (callback.borrow())(mode);
        }
    }

    /// Turns the dial one step.
    pub fn cycle_permission_mode(&mut self) {
        self.set_permission_mode(self.permission_mode.next());
    }

    /// The messages the session on screen holds for later.
    pub fn queued_messages(&self) -> Vec<crate::composer::Queued> {
        self.active_session_id()
            .and_then(|id| self.queues.get(id))
            .map(|queue| queue.items().to_vec())
            .unwrap_or_default()
    }

    /// Takes a queued message out of the session on screen's queue.
    pub fn take_queued(&mut self, queued: u64) -> Option<String> {
        let id = self.active_session_id()?.clone();
        self.queues.get_mut(&id)?.take(queued)
    }

    /// Sends a queued message into the running turn now.
    pub fn send_queued_now(&mut self, queued: u64) -> bool {
        let Some(id) = self.active_session_id().cloned() else {
            return false;
        };
        let Some(text) = self.take_queued(queued) else {
            return false;
        };
        let delivery = match submit(self.turn_phase(), true) {
            Submit::Deliver(delivery) => delivery,
            Submit::Queue => Delivery::Steer,
        };
        self.deliver(&id, &text, delivery);
        true
    }

    /// Sends the next queued message of every session that has become idle.
    /// One message per turn, and never to a session that is stopped on the
    /// user: a queued message is not the user's answer.
    pub fn drain_queues(&mut self) {
        let active = self.active_session_id().cloned();
        let active_phase = self.turn_phase();
        let ids: Vec<SessionId> = self.queues.keys().cloned().collect();
        for id in ids {
            let phase = if Some(&id) == active.as_ref() {
                active_phase
            } else {
                // Off screen only the session's status is known.
                match self.store().session(&id).map(|session| session.status()) {
                    Some(SessionStatus::Idle) => TurnPhase::Idle,
                    Some(SessionStatus::Running) => TurnPhase::Generating,
                    Some(SessionStatus::AwaitingUser) => TurnPhase::AwaitingApproval,
                    Some(SessionStatus::Error) | None => TurnPhase::Error,
                }
            };
            let next = self.queues.get_mut(&id).and_then(|queue| queue.next_for(phase));
            if let Some(text) = next {
                self.deliver(&id, &text, Delivery::Send);
            }
        }
        self.queues.retain(|_, queue| !queue.is_empty() || queue.is_awaiting_turn());
    }

    /// Submits what is in the composer and clears it.
    pub(crate) fn submit_composer(
        &mut self,
        immediate: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(composer) = self.composer.clone() else {
            return;
        };
        let text = composer.read(cx).value().to_string();
        if self.submit_message(&text, immediate).is_some() {
            composer.update(cx, |state, cx| state.set_value("", window, cx));
            self.composer_has_text = false;
            cx.notify();
        }
    }
}

// ---------------------------------------------------------------------------
// Drawing.

fn step_color(status: StepStatus, p: Palette) -> Hsla {
    match status {
        StepStatus::Pending => p.muted,
        StepStatus::InProgress => p.info,
        StepStatus::Done => p.success,
    }
}

/// A section of the activity bar: separated from what is above it.
fn section(p: Palette) -> Div {
    div()
        .flex_none()
        .px_2()
        .py_1()
        .border_t_1()
        .border_color(p.border)
        .flex()
        .flex_col()
        .gap_1()
}

/// A button of the activity bar: a word in a bordered pill.
fn action(id: impl Into<gpui::ElementId>, label: &'static str, color: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .px_2()
        .py_1()
        .rounded(px(4.))
        .border_1()
        .border_color(color.opacity(0.5))
        .text_xs()
        .text_color(color)
        .cursor_pointer()
        .child(label)
}

/// A small icon-only button.
fn icon_action(id: impl Into<gpui::ElementId>, which: Icon, color: Hsla) -> Stateful<Div> {
    div()
        .id(id)
        .p_1()
        .rounded(px(4.))
        .cursor_pointer()
        .child(icon(which, color))
}

impl<S: SessionView> HarnessShell<S> {
    /// The activity bar for `ws`: only the parts that have something to say.
    pub(crate) fn activity_bar(&self, ws: &S::Workspace, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let mut bar = div().flex_none().flex().flex_col();

        if ws.plan_phase() != PlanPhase::None || !ws.plan().is_empty() {
            bar = bar.child(self.plan_section(ws, p, cx));
        }
        match ws.plan_phase() {
            PlanPhase::Contingent => bar = bar.child(self.decision_section(ws, p)),
            PlanPhase::Clarify => bar = bar.child(self.clarify_section(ws, p)),
            _ => {}
        }
        if let Some(approval) = self.pending_approval() {
            bar = bar.child(self.approval_section(&approval, p, cx));
        }
        if !self.queued_messages().is_empty() {
            bar = bar.child(self.queue_section(p, cx));
        }
        bar.into_any_element()
    }

    /// The plan: one line of where it stands, unfolding to its steps.
    fn plan_section(&self, ws: &S::Workspace, p: Palette, cx: &mut Context<Self>) -> Div {
        let plan = ws.plan();
        let (done, total) = plan_progress(&plan);
        let current = plan
            .iter()
            .find(|step| step.status == StepStatus::InProgress)
            .map(|step| step.text.clone());
        let open = self.plan_open;
        let summary = self.trf(
            "shell.plan_progress",
            &[("done", &done.to_string()), ("total", &total.to_string())],
        );
        section(p)
            .child(
                div()
                    .id("harness-plan-summary")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            this.plan_open = !this.plan_open;
                            cx.notify();
                        }),
                    )
                    .child(icon(Icon::chevron(open), p.muted))
                    .child(icon(Icon::Plan, p.muted))
                    .child(summary)
                    .child(
                        div()
                            .text_color(p.muted)
                            .child(ws.plan_phase().label(self.locale)),
                    )
                    .when_some(current.filter(|_| !open), |d, current| {
                        d.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(p.muted)
                                .child(current),
                        )
                    })
                    // The product's own progress (a computation under way).
                    .when_some(ws.progress(), |d, (done, total)| {
                        let share = if total > 0 {
                            (done as f32 / total as f32).clamp(0., 1.)
                        } else {
                            0.
                        };
                        d.child(
                            div()
                                .w(px(64.))
                                .h(px(4.))
                                .flex_none()
                                .rounded(px(2.))
                                .bg(p.border)
                                .child(
                                    div()
                                        .w(px(64. * share))
                                        .h(px(4.))
                                        .rounded(px(2.))
                                        .bg(p.info),
                                ),
                        )
                    }),
            )
            .when(open, |d| {
                d.children(plan.into_iter().map(|step| {
                    div()
                        .pl_6()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(
                            div()
                                .w(px(8.))
                                .h(px(8.))
                                .flex_none()
                                .rounded(px(4.))
                                .bg(step_color(step.status, p)),
                        )
                        .child(div().flex_1().text_xs().child(step.text))
                        .child(
                            div()
                                .text_xs()
                                .text_color(p.muted)
                                .child(step.status.label(self.locale)),
                        )
                }))
            })
    }

    /// A plan that failed verification and waits for the user's decision.
    fn decision_section(&self, ws: &S::Workspace, p: Palette) -> Div {
        let mut buttons = div().flex().flex_row().flex_wrap().items_center().gap_2();
        for (key, decision, color) in [
            ("shell.accept", PlanDecision::Accept, p.success),
            ("shell.reject", PlanDecision::Reject, p.danger),
            ("shell.modify", PlanDecision::Modify, p.fg),
        ] {
            let callback = self.on_plan_decision.clone();
            let session_id = ws.session_id().clone();
            buttons = buttons.child(action(key, self.tr(key), color).on_click(
                move |_, _, _| {
                    if let Some(callback) = callback.clone() {
                        (callback.borrow())(&session_id, decision);
                    }
                },
            ));
        }
        // What the product reports settled item by item.
        for (name, accepted) in ws.settlement_items() {
            let (mark, color) = if accepted {
                (Icon::CheckPass, p.success)
            } else {
                (Icon::CheckFail, p.danger)
            };
            buttons = buttons.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(color)
                    .child(icon(mark, color))
                    .child(name),
            );
        }
        section(p)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(p.warning)
                    .child(icon(Icon::Error, p.warning))
                    .child(self.trf(
                        "shell.plan_rejected",
                        &[("index", ws.plan_phase().label(self.locale))],
                    )),
            )
            .child(buttons)
    }

    /// The agent needs something answered before it can go on. The answer is
    /// typed in the composer.
    fn clarify_section(&self, ws: &S::Workspace, p: Palette) -> Div {
        section(p)
            .child(
                div()
                    .text_xs()
                    .text_color(p.warning)
                    .child(self.tr("shell.clarify_prompt")),
            )
            .children(
                ws.clarify_questions()
                    .into_iter()
                    .enumerate()
                    .map(|(ix, question)| div().text_sm().child(format!("{}. {question}", ix + 1))),
            )
    }

    /// A tool call waiting for the user: what it is, and the three answers.
    fn approval_section(&self, approval: &Approval, p: Palette, cx: &mut Context<Self>) -> Div {
        let decide = |decision: ApprovalDecision| {
            cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                this.decide_approval(decision.clone());
                cx.notify();
            })
        };
        let mono = cx.theme().mono_font_family.clone();
        let buttons = if self.always_scope_open {
            // "Always" first says how far it reaches, then asks which.
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .child(self.tr("approval.always_scope")),
                )
                .child(
                    action(
                        "harness-approve-session",
                        self.tr("approval.scope_session"),
                        p.success,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        decide(ApprovalDecision::AllowAlways(ApprovalScope::Session)),
                    ),
                )
                .child(
                    action("harness-approve-kind", self.tr("approval.scope_kind"), p.warning)
                        .on_mouse_down(
                            MouseButton::Left,
                            decide(ApprovalDecision::AllowAlways(ApprovalScope::Kind)),
                        ),
                )
                .child(
                    action("harness-approve-back", self.tr("approval.cancel"), p.muted)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                this.always_scope_open = false;
                                cx.notify();
                            }),
                        ),
                )
        } else {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .items_center()
                .gap_2()
                .child(
                    action("harness-approve-once", self.tr("approval.allow_once"), p.success)
                        .on_mouse_down(MouseButton::Left, decide(ApprovalDecision::AllowOnce)),
                )
                .child(
                    action("harness-approve-always", self.tr("approval.allow_always"), p.fg)
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                this.always_scope_open = true;
                                cx.notify();
                            }),
                        ),
                )
                .child(
                    action("harness-approve-reject", self.tr("approval.reject"), p.danger)
                        .on_mouse_down(
                            MouseButton::Left,
                            decide(ApprovalDecision::Reject { feedback: None }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(p.muted)
                        .child(self.tr("approval.type_to_refuse")),
                )
        };
        section(p)
            .bg(p.warning.opacity(0.06))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(icon(Icon::tool(approval.kind), p.warning))
                    .child(
                        div()
                            .text_color(p.warning)
                            .child(self.tr("approval.needs_you")),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(mono.clone())
                            .child(approval.title.clone()),
                    ),
            )
            .when_some(approval.detail.clone(), |d, detail| {
                // Capped, so a long call cannot push the conversation away.
                d.child(
                    div()
                        .id("harness-approval-detail")
                        .max_h(px(96.))
                        .overflow_y_scroll()
                        .font_family(mono)
                        .text_xs()
                        .text_color(p.muted)
                        .child(detail),
                )
            })
            .child(buttons)
    }

    /// The messages held until the turn ends.
    fn queue_section(&self, p: Palette, cx: &mut Context<Self>) -> Div {
        let queued = self.queued_messages();
        let open = self.queue_open;
        section(p)
            .child(
                div()
                    .id("harness-queue-summary")
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            this.queue_open = !this.queue_open;
                            cx.notify();
                        }),
                    )
                    .child(icon(Icon::chevron(open), p.muted))
                    .child(icon(Icon::Queue, p.muted))
                    .child(self.trf("queue.count", &[("count", &queued.len().to_string())])),
            )
            .when(open, |d| {
                d.children(queued.into_iter().map(|message| {
                    let id = message.id;
                    div()
                        .pl_6()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .child(message.text),
                        )
                        // Back into the composer, to change it.
                        .child(
                            icon_action(("harness-queue-edit", id as usize), Icon::ToolEdit, p.muted)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                                        if let (Some(text), Some(composer)) =
                                            (this.take_queued(id), this.composer.clone())
                                        {
                                            composer.update(cx, |state, cx| {
                                                state.set_value(text, window, cx);
                                            });
                                            this.composer_has_text = true;
                                        }
                                        cx.notify();
                                    }),
                                ),
                        )
                        // Into the running turn, now.
                        .child(
                            icon_action(("harness-queue-send", id as usize), Icon::Send, p.muted)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                        this.send_queued_now(id);
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(
                            icon_action(("harness-queue-drop", id as usize), Icon::Close, p.muted)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                        this.take_queued(id);
                                        cx.notify();
                                    }),
                                ),
                        )
                }))
            })
    }

    /// The composer: the text area with its one button, and the line below it
    /// that says what the agent is running as.
    pub(crate) fn composer_area(&self, ws: &S::Workspace, cx: &mut Context<Self>) -> AnyElement {
        let p = palette(cx);
        let Some(composer) = self.composer.clone() else {
            return div().into_any_element();
        };
        let act = ws.turn_phase().composer_action(self.composer_has_text);
        let (button_icon, button_color) = match act {
            ComposerAction::Send => (Icon::Send, p.accent),
            ComposerAction::Queue => (Icon::Queue, p.info),
            ComposerAction::Stop => (Icon::Stop, p.danger),
        };
        // Nothing to send and nothing to stop: the button rests.
        let idle = act == ComposerAction::Send && !self.composer_has_text;
        let mode = self.permission_mode;
        let mode_color = if mode.is_unguarded() { p.warning } else { p.muted };
        let context = ws.context_usage().map(|used| {
            let color = match ContextLevel::of(used) {
                ContextLevel::Normal => p.muted,
                ContextLevel::Warning => p.warning,
                ContextLevel::Critical => p.danger,
            };
            let percent = (used * 100.).round().clamp(0., 999.) as u32;
            (self.trf("composer.context", &[("percent", &percent.to_string())]), color)
        });

        div()
            .flex_none()
            .border_t_1()
            .border_color(p.border)
            .flex()
            .flex_col()
            .child(
                div()
                    .p_2()
                    .flex()
                    .flex_row()
                    .items_end()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(Textarea::new(&composer)))
                    .child(
                        icon_action("harness-composer-button", button_icon, button_color)
                            .when(idle, |d| d.opacity(0.4))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                                    match act {
                                        ComposerAction::Stop => {
                                            this.stop_turn();
                                        }
                                        ComposerAction::Send | ComposerAction::Queue => {
                                            this.submit_composer(false, window, cx);
                                        }
                                    }
                                    cx.notify();
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .px_3()
                    .pb_1()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .text_xs()
                    .text_color(p.muted)
                    .when_some(ws.model_label(), |d, model| d.child(model))
                    .child(
                        div()
                            .id("harness-permission-mode")
                            .cursor_pointer()
                            .text_color(mode_color)
                            .child(mode.label(self.locale))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                    this.cycle_permission_mode();
                                    cx.notify();
                                }),
                            ),
                    )
                    .child(div().flex_1())
                    .when_some(context, |d, (label, color)| {
                        d.child(div().text_color(color).child(label))
                    }),
            )
            .into_any_element()
    }
}
