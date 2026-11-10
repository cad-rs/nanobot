//! The conversation stream (docs/ui-redesign-design.md §5.2): the transcript
//! as rows of a virtualised list.
//!
//! An entry is not always a row. An assistant answer is cut into rows of about
//! [`SEGMENT_BYTES`] at paragraph breaks, because a markdown view costs what
//! is laid out on screen and one view per answer stops fitting a frame at
//! around 30 KB; cut up, the list draws only the rows in sight. And tool calls
//! that are only a line each are gathered, run by run, into one row that
//! unfolds — a turn is mostly reads and searches, and they should not bury
//! what it did.
//!
//! [`rows`] does that flattening, [`sync`] says how a new row list differs
//! from the one on screen (which rows to remeasure, where to splice), and
//! [`StreamState`] applies it to the list. Those three are plain data and are
//! tested without a window; the drawing is at the end of the file.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    div, list, prelude::*, px, AnyElement, Context, Entity, FollowMode, Hsla, ListAlignment,
    ListState, MouseButton, MouseDownEvent, SharedString,
};
use gpui_component::text::{TextView, TextViewState};
use gpui_component::ActiveTheme as _;

use crate::i18n::Locale;
use crate::icons::{icon, Icon};
use crate::model::{PlanStep, SessionId, SessionView};
use crate::motion::{self, Mark};
use crate::activity::step_color;
use crate::shell::{badge, palette, HarnessShell, Palette};
use crate::transcript::{Entry, EntryKind, ToolCall, ToolKind, ToolStatus};

/// About how much markdown one row holds.
pub const SEGMENT_BYTES: usize = 2000;
/// How many lines of a tool's output show before it is unfolded.
pub const OUTPUT_TAIL_LINES: usize = 5;

/// Where to cut `text` into rows: at the first blank line after each
/// `target` bytes, never inside a fenced code block.
///
/// A cut depends only on the text before it, so the cuts of a streaming
/// answer never move as it grows: only the last row changes.
pub fn split_markdown(text: &str, target: usize) -> Vec<Range<usize>> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    let mut in_fence = false;
    let mut after_blank = false;
    for line in text.split_inclusive('\n') {
        let line_start = offset;
        offset += line.len();
        let trimmed = line.trim();
        let was_in_fence = in_fence;
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_fence = !in_fence;
        }
        let blank = trimmed.is_empty();
        // A block starts here: the first line after a run of blank ones.
        if after_blank && !blank && !was_in_fence && line_start - start >= target {
            segments.push(start..line_start);
            start = line_start;
        }
        // A blank line inside a fence is code, not a break.
        after_blank = blank && !was_in_fence;
    }
    if start < text.len() || segments.is_empty() {
        segments.push(start..text.len());
    }
    segments
}

/// What identifies a row across projections: the event it came from, and
/// which part of that event it is (the segment of an answer; 0 otherwise).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct RowKey {
    pub seq: u64,
    pub part: u32,
}

impl RowKey {
    fn of(seq: u64) -> Self {
        Self { seq, part: 0 }
    }
}

/// What a row shows.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RowKind {
    User {
        text: String,
    },
    /// One segment of an answer.
    Markdown {
        text: String,
        /// The last segment of an answer still arriving.
        streaming: bool,
    },
    Thinking {
        text: String,
        duration_ms: Option<u64>,
    },
    /// A run of tool calls that are a line each, with the event each came from.
    Tools(Vec<(u64, ToolCall)>),
    /// A tool call that is a card.
    ToolCard(ToolCall),
    Plan {
        steps: Vec<PlanStep>,
    },
    Check {
        name: String,
        passed: bool,
        details: Option<String>,
    },
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

/// One row of the stream.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    pub key: RowKey,
    pub kind: RowKind,
    /// The product object the row's entry is about.
    pub anchor: Option<String>,
}

/// The transcript as rows, in order.
pub fn rows(entries: &[Entry]) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for entry in entries {
        let key = RowKey::of(entry.seq);
        let row = |kind| Row {
            key,
            kind,
            anchor: entry.anchor.clone(),
        };
        match &entry.kind {
            EntryKind::User { text } => rows.push(row(RowKind::User { text: text.clone() })),
            EntryKind::Assistant { markdown, streaming } => {
                let segments = split_markdown(markdown, SEGMENT_BYTES);
                let last = segments.len() - 1;
                for (part, range) in segments.into_iter().enumerate() {
                    rows.push(Row {
                        key: RowKey {
                            seq: entry.seq,
                            part: part as u32,
                        },
                        kind: RowKind::Markdown {
                            text: markdown[range].to_string(),
                            streaming: *streaming && part == last,
                        },
                        // The chip goes with the answer's first row.
                        anchor: entry.anchor.clone().filter(|_| part == 0),
                    });
                }
            }
            EntryKind::Thinking { text, duration_ms } => rows.push(row(RowKind::Thinking {
                text: text.clone(),
                duration_ms: *duration_ms,
            })),
            EntryKind::Tool(call) if call.is_card() => {
                rows.push(row(RowKind::ToolCard(call.clone())));
            }
            EntryKind::Tool(call) => match rows.last_mut() {
                // Join the run this call continues.
                Some(Row {
                    kind: RowKind::Tools(calls),
                    ..
                }) => calls.push((entry.seq, call.clone())),
                _ => rows.push(Row {
                    key,
                    kind: RowKind::Tools(vec![(entry.seq, call.clone())]),
                    anchor: None,
                }),
            },
            EntryKind::Plan { steps } => rows.push(row(RowKind::Plan {
                steps: steps.clone(),
            })),
            EntryKind::Check {
                name,
                passed,
                details,
            } => rows.push(row(RowKind::Check {
                name: name.clone(),
                passed: *passed,
                details: details.clone(),
            })),
            EntryKind::TurnEnd { summary } => rows.push(row(RowKind::TurnEnd {
                summary: summary.clone(),
            })),
            EntryKind::Error { title, message, retryable } => rows.push(row(RowKind::Error {
                retryable: *retryable,
                title: title.clone(),
                message: message.clone(),
            })),
        }
    }
    rows
}

/// How a new row list differs from the one on screen.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct RowSync {
    /// Rows that are the same row with new content: remeasure them.
    pub changed: Vec<usize>,
    /// Replace the rows in this range with this many new ones.
    pub splice: Option<(Range<usize>, usize)>,
}

impl RowSync {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.splice.is_none()
    }
}

/// Rows keep their place while their keys agree from the start; from the first
/// key that differs, the old rows go and the new ones come.
pub fn sync(old: &[Row], new: &[Row]) -> RowSync {
    let shared = old
        .iter()
        .zip(new)
        .take_while(|(old, new)| old.key == new.key)
        .count();
    RowSync {
        changed: (0..shared).filter(|&ix| old[ix] != new[ix]).collect(),
        splice: (old.len() > shared || new.len() > shared)
            .then(|| (shared..old.len(), new.len() - shared)),
    }
}

/// The stream on screen: its rows, the list that draws them and what the user
/// unfolded.
pub struct StreamState {
    rows: Vec<Row>,
    pub(crate) list: ListState,
    /// The markdown views, kept across projections so a growing answer is
    /// appended to rather than parsed again.
    cells: HashMap<RowKey, Entity<TextViewState>>,
    expanded: HashSet<RowKey>,
    /// Whether the list was following its end at the last scroll: the shell
    /// redraws when that flips, to show or hide the way back to the end.
    following_seen: Rc<Cell<bool>>,
}

impl Default for StreamState {
    fn default() -> Self {
        let list = ListState::new(0, ListAlignment::Top, px(1000.));
        // New content keeps the end in view until the user scrolls away.
        list.set_follow_mode(FollowMode::Tail);
        Self {
            rows: Vec::new(),
            list,
            cells: HashMap::new(),
            expanded: HashSet::new(),
            following_seen: Rc::new(Cell::new(true)),
        }
    }
}

impl StreamState {
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Brings the rows up to `entries`, telling the list only what changed.
    pub fn set_entries(&mut self, entries: &[Entry]) -> RowSync {
        let new = rows(entries);
        let change = sync(&self.rows, &new);
        for &ix in &change.changed {
            self.list.remeasure_items(ix..ix + 1);
        }
        if let Some((range, count)) = &change.splice {
            self.list.splice(range.clone(), *count);
        }
        self.rows = new;
        // Forget what belonged to rows that are gone.
        let live: HashSet<RowKey> = self.rows.iter().map(|row| row.key).collect();
        self.cells.retain(|key, _| live.contains(key));
        self.expanded.retain(|key| live.contains(key));
        change
    }

    pub fn is_expanded(&self, key: RowKey) -> bool {
        self.expanded.contains(&key)
    }

    /// Folds or unfolds a row; its height changes, so it is remeasured.
    pub fn toggle(&mut self, key: RowKey) {
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        if let Some(ix) = self.rows.iter().position(|row| row.key == key) {
            self.list.remeasure_items(ix..ix + 1);
        }
    }

    /// Whether new content keeps the end in view.
    pub fn is_following(&self) -> bool {
        self.list.is_following_tail()
    }

    /// Back to the end, following again.
    pub fn follow(&self) {
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// Whether the list shows its very end. When the list cannot say — the
    /// content does not fill it, or rows below are not measured yet — it does
    /// if it is following its end and not otherwise.
    pub fn at_end(&self) -> bool {
        self.list
            .is_scrolled_to_end()
            .unwrap_or_else(|| self.is_following())
    }

    /// Where the list is scrolled to: the row at the top and how far into it.
    pub fn scroll_position(&self) -> (usize, f32) {
        let top = self.list.logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    }

    /// Unfolds every tool row, or folds them all when none is left to
    /// unfold. True when there was a tool row at all.
    pub fn toggle_tools(&mut self) -> bool {
        let tools: Vec<RowKey> = self
            .rows
            .iter()
            .filter(|row| matches!(row.kind, RowKind::Tools(_) | RowKind::ToolCard(_)))
            .map(|row| row.key)
            .collect();
        if tools.is_empty() {
            return false;
        }
        if tools.iter().all(|key| self.expanded.contains(key)) {
            for key in &tools {
                self.expanded.remove(key);
            }
        } else {
            self.expanded.extend(tools);
        }
        self.list.remeasure();
        true
    }

    /// Scrolls to the user's message `step` away from the top of what is
    /// shown (-1: the one before, 1: the one after). False when there is none
    /// that way.
    pub fn step_user_message(&self, step: isize) -> bool {
        let top = self.list.logical_scroll_top().item_ix;
        match user_message_from(&self.rows, top, step) {
            Some(ix) => {
                self.list.scroll_to(gpui::ListOffset {
                    item_ix: ix,
                    offset_in_item: px(0.),
                });
                true
            }
            None => false,
        }
    }
}

/// What a run of one-line tool calls amounts to: its title for one (the row
/// leads with the kind's icon), the
/// count per kind for more ("read ×3 · search ×2"), in the order met.
pub fn tools_summary(calls: &[(u64, ToolCall)], locale: Locale) -> String {
    if let [(_, call)] = calls {
        return call.title.clone();
    }
    let mut counts: Vec<(ToolKind, usize)> = Vec::new();
    for (_, call) in calls {
        match counts.iter_mut().find(|(kind, _)| *kind == call.kind) {
            Some((_, count)) => *count += 1,
            None => counts.push((call.kind, 1)),
        }
    }
    counts
        .into_iter()
        .map(|(kind, count)| format!("{} ×{count}", kind_label(kind, locale)))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The state a run of calls is in as a whole: the most pressing of them.
pub fn tools_status(calls: &[(u64, ToolCall)]) -> ToolStatus {
    let any = |status| calls.iter().any(|(_, call)| call.status == status);
    [
        ToolStatus::Running,
        ToolStatus::Failed,
        ToolStatus::Rejected,
        ToolStatus::Cancelled,
    ]
    .into_iter()
    .find(|&status| any(status))
    .unwrap_or(ToolStatus::Done)
}

pub fn kind_label(kind: ToolKind, locale: Locale) -> &'static str {
    crate::i18n::t(
        locale,
        match kind {
            ToolKind::Read => "toolkind.read",
            ToolKind::Edit => "toolkind.edit",
            ToolKind::Delete => "toolkind.delete",
            ToolKind::Move => "toolkind.move",
            ToolKind::Search => "toolkind.search",
            ToolKind::Execute => "toolkind.execute",
            ToolKind::Think => "toolkind.think",
            ToolKind::Fetch => "toolkind.fetch",
            ToolKind::Other => "toolkind.other",
        },
    )
}

/// The last `lines` lines of `output`, and how many came before them.
pub fn output_tail(output: &str, lines: usize) -> (&str, usize) {
    let total = output.lines().count();
    if total <= lines {
        return (output, 0);
    }
    let hidden = total - lines;
    let start = output
        .split_inclusive('\n')
        .take(hidden)
        .map(str::len)
        .sum();
    (&output[start..], hidden)
}

/// The row of the user's message `step` away from row `top` (-1: the nearest
/// one before it, 1: the nearest one after it).
pub fn user_message_from(rows: &[Row], top: usize, step: isize) -> Option<usize> {
    let is_user = |ix: &usize| matches!(rows[*ix].kind, RowKind::User { .. });
    if step < 0 {
        (0..top.min(rows.len())).rev().find(is_user)
    } else {
        (top + 1..rows.len()).find(is_user)
    }
}

/// What a line of a diff is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DiffLineKind {
    Added,
    Removed,
    Context,
    /// A hunk header (`@@ … @@`) or a file header (`---`, `+++`).
    Header,
}

/// The lines of a unified diff, each without its marker, and how many were
/// added and removed. Headers count as neither.
pub fn diff_lines(diff: &str) -> (Vec<(DiffLineKind, &str)>, usize, usize) {
    let (mut added, mut removed) = (0, 0);
    let lines = diff
        .lines()
        .map(|line| {
            if line.starts_with("@@") || line.starts_with("+++") || line.starts_with("---") {
                (DiffLineKind::Header, line)
            } else if let Some(rest) = line.strip_prefix('+') {
                added += 1;
                (DiffLineKind::Added, rest)
            } else if let Some(rest) = line.strip_prefix('-') {
                removed += 1;
                (DiffLineKind::Removed, rest)
            } else {
                (DiffLineKind::Context, line.strip_prefix(' ').unwrap_or(line))
            }
        })
        .collect();
    (lines, added, removed)
}

/// How many lines of a diff a folded card shows.
pub const DIFF_HEAD_LINES: usize = 12;

/// A duration as the thinking row states it: "0.8s", "12s", "2m 05s".
pub fn duration_label(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else if secs >= 10 {
        format!("{secs}s")
    } else {
        format!("{:.1}s", ms as f64 / 1000.)
    }
}

// ---------------------------------------------------------------------------
// Drawing.

fn tool_color(status: ToolStatus, p: Palette) -> Hsla {
    match status {
        ToolStatus::AwaitingApproval => p.warning,
        ToolStatus::Running => p.info,
        ToolStatus::Done => p.success,
        ToolStatus::Failed => p.danger,
        ToolStatus::Cancelled | ToolStatus::Rejected => p.muted,
    }
}

impl<S: SessionView> HarnessShell<S> {
    /// What pointing at an object does: the session, and the object's name
    /// as the entry gave it. With this set, the chip under an entry is a
    /// button.
    pub fn set_on_anchor<F: Fn(&SessionId, &str) + 'static>(&mut self, callback: F) {
        self.on_anchor = Some(std::rc::Rc::new(callback));
    }

    /// Asks the product to show `anchor`. In a stacked layout the viewport
    /// takes the window, so what was pointed at can be seen.
    pub fn show_anchor(&mut self, anchor: &str) -> bool {
        match (self.on_anchor.clone(), self.active_session_id().cloned()) {
            (Some(callback), Some(session)) => {
                callback(&session, anchor);
                self.layout.set_stacked(crate::layout::Stacked::Viewport);
                true
            }
            _ => false,
        }
    }

    /// What trying again after an error does: the session, and the sequence
    /// of the error entry.
    pub fn set_on_retry<F: Fn(&SessionId, u64) + 'static>(&mut self, callback: F) {
        self.on_retry = Some(std::rc::Rc::new(callback));
    }

    /// Whether the error at `seq` can be tried again: the entry says trying
    /// again could help, the product gave a way,
    /// nothing came after the error, and no turn is running.
    pub fn can_retry(&self, seq: u64) -> bool {
        self.on_retry.is_some()
            && !self.turn_phase().is_busy()
            && self.stream.rows().last().is_some_and(|row| {
                row.key.seq == seq && matches!(row.kind, RowKind::Error { retryable: true, .. })
            })
    }

    /// Asks the product to try again after the error at `seq`.
    pub fn retry(&mut self, seq: u64) -> bool {
        if !self.can_retry(seq) {
            return false;
        }
        match (self.on_retry.clone(), self.active_session_id().cloned()) {
            (Some(callback), Some(session)) => {
                callback(&session, seq);
                true
            }
            _ => false,
        }
    }

    /// The conversation stream: the list, and the way back to its end once the
    /// user has scrolled away from it.
    pub(crate) fn render_stream(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let following = self.stream.is_following();
        // Scrolling does not redraw the shell by itself; it must when the
        // list stops or resumes following, or the button below would lag.
        let (shell, seen) = (cx.entity_id(), self.stream.following_seen.clone());
        seen.set(following);
        self.stream.list.set_scroll_handler(move |event, _window, cx| {
            if seen.replace(event.is_following_tail) != event.is_following_tail {
                cx.notify(shell);
            }
        });
        let stream = list(
            self.stream.list.clone(),
            cx.processor(|this: &mut Self, ix: usize, _window, cx| {
                this.render_row(ix, cx)
            }),
        )
        .size_full();
        div()
            .id("harness-conversation")
            .relative()
            .flex_1()
            .min_h_0()
            .child(stream)
            .when(self.stream.rows().is_empty(), |d| {
                d.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_sm()
                        .text_color(p.muted)
                        .child(self.tr("stream.empty")),
                )
            })
            .when(!following && !self.stream.rows().is_empty(), |d| {
                d.child(
                    div()
                        .id("harness-jump-to-end")
                        .absolute()
                        .bottom(px(8.))
                        .right(px(16.))
                        .px_2()
                        .py_1()
                        .rounded(px(6.))
                        .border_1()
                        .border_color(p.border)
                        .bg(cx.theme().background)
                        .text_xs()
                        .text_color(p.muted)
                        .cursor_pointer()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_1()
                        .child(icon(Icon::ArrowDown, p.muted))
                        .child(self.tr("stream.jump_to_end"))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                this.stream.follow();
                                cx.notify();
                            }),
                        ),
                )
            })
    }

    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.stream.rows().get(ix).cloned() else {
            return div().into_any_element();
        };
        let p = palette(cx);
        let key = row.key;
        let open = self.stream.is_expanded(key);
        let toggle = cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
            this.stream.toggle(key);
            cx.notify();
        });
        let mono = cx.theme().mono_font_family.clone();
        let rewindable = self.on_rewind.is_some();
        let still = self.still(cx);
        let is_last = ix + 1 == self.stream.rows().len();

        let body = match row.kind {
            RowKind::User { text } => div()
                .id(("user-message", ix))
                .px_2()
                .py_3()
                .rounded(px(6.))
                .border_1()
                .border_color(p.border)
                .bg(cx.theme().secondary)
                .font_family(mono)
                .text_xs()
                // A click takes the message back into the composer, to go
                // back to it and say it differently.
                .when(rewindable, |d| {
                    d.cursor_pointer().on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                            this.begin_rewind_in(key.seq, window, cx);
                        }),
                    )
                })
                .when(self.rewinding == Some(key.seq), |d| d.border_color(p.info))
                .child(text)
                .into_any_element(),
            RowKind::Markdown { text, streaming } => {
                let state = match self.stream.cells.get(&key) {
                    Some(state) => state.clone(),
                    None => {
                        let state = cx.new(|cx| TextViewState::markdown(&text, cx));
                        self.stream.cells.insert(key, state.clone());
                        state
                    }
                };
                // An answer that grew is appended to; the same text is a no-op.
                state.update(cx, |state, cx| state.set_text(&text, cx));
                div()
                    .px_3()
                    .text_sm()
                    .child(TextView::new(&state).selectable(true))
                    // The answer is still being written.
                    .when(streaming, |d| {
                        d.flex().flex_col().gap_1().child(motion::dots(
                            ("answer-dots", ix),
                            p.muted,
                            still,
                        ))
                    })
                    .into_any_element()
            }
            RowKind::Thinking { text, duration_ms } => {
                let thinking_now =
                    duration_ms.is_none() && is_last && self.turn_phase().is_busy();
                let label = match duration_ms {
                    Some(ms) => format!("{} · {}", self.tr("entry.thinking"), duration_label(ms)),
                    None => self.tr("entry.thinking").to_string(),
                };
                div()
                    .px_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            .text_xs()
                            .text_color(p.muted)
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, toggle)
                            .child(icon(Icon::chevron(open), p.muted))
                            .child(icon(Icon::Thinking, p.muted))
                            // Still thinking: the label stands in for the
                            // thought to come.
                            .child(if thinking_now {
                                motion::shimmer(("thinking-label", ix), div().child(label), still)
                            } else {
                                div().child(label).into_any_element()
                            }),
                    )
                    .when(open, |d| {
                        d.child(
                            div()
                                .id(("thinking", ix))
                                .ml_1()
                                .pl_3()
                                .border_l_1()
                                .border_color(p.border)
                                .max_h(px(256.))
                                .overflow_y_scroll()
                                .text_xs()
                                .text_color(p.muted)
                                .child(text),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Tools(calls) => {
                let status = tools_status(&calls);
                let summary = tools_summary(&calls, self.locale);
                let foldable = calls.len() > 1 || calls.iter().any(|(_, c)| c.output.is_some());
                // A run leads with its fold mark; a single call with its kind.
                let lead = match calls.as_slice() {
                    [(_, call)] if !foldable => Icon::tool(call.kind),
                    _ => Icon::chevron(open),
                };
                let locale = self.locale;
                div()
                    .px_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .text_color(p.muted)
                            .when(foldable, |d| {
                                d.cursor_pointer().on_mouse_down(MouseButton::Left, toggle)
                            })
                            .child(icon(lead, p.muted))
                            .child(div().flex_1().min_w_0().truncate().child(summary))
                            .children(motion::mark(
                                Mark::of_tool(status),
                                ("tools-mark", ix),
                                tool_color(status, p),
                                still,
                            ))
                            .when(status != ToolStatus::Done, |d| {
                                d.child(
                                    div()
                                        .text_color(tool_color(status, p))
                                        .child(status.label(locale)),
                                )
                            }),
                    )
                    .when(open && foldable, |d| {
                        d.child(
                            div()
                                .ml_1()
                                .pl_3()
                                .border_l_1()
                                .border_color(p.border)
                                .flex()
                                .flex_col()
                                .gap_1()
                                .children(calls.into_iter().map(|(_, call)| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .text_xs()
                                        .child(
                                            div()
                                                .flex()
                                                .flex_row()
                                                .items_center()
                                                .text_color(p.muted)
                                                .child(icon(Icon::tool(call.kind), p.muted))
                                                .child(
                                                    div()
                                                        .flex_1()
                                                        .min_w_0()
                                                        .truncate()
                                                        .child(call.title),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(tool_color(call.status, p))
                                                        .child(call.status.label(locale)),
                                                ),
                                        )
                                        .when_some(call.output, |d, output| {
                                            d.child(
                                                div()
                                                    .font_family(mono.clone())
                                                    .text_color(p.muted)
                                                    .child(output),
                                            )
                                        })
                                })),
                        )
                    })
                    .into_any_element()
            }
            RowKind::ToolCard(call) => {
                let color = tool_color(call.status, p);
                let over = matches!(
                    call.status,
                    ToolStatus::Failed | ToolStatus::Cancelled | ToolStatus::Rejected
                );
                let output = call.output.as_deref().unwrap_or("");
                let (tail, hidden) = if open {
                    (output, 0)
                } else {
                    output_tail(output, OUTPUT_TAIL_LINES)
                };
                let tail = tail.trim_end().to_string();
                let diff_text = call.diff.as_deref().unwrap_or("");
                let (diff, added, removed) = diff_lines(diff_text);
                let diff_hidden = if open { 0 } else { diff.len().saturating_sub(DIFF_HEAD_LINES) };
                let has_more = hidden > 0 || diff_hidden > 0 || call.input.is_some();
                div()
                    .mx_2()
                    .rounded(px(6.))
                    .border_1()
                    // Waiting for the user stands out; ended badly is dashed,
                    // so it reads without colour too.
                    .border_color(if call.status == ToolStatus::AwaitingApproval {
                        p.warning
                    } else {
                        p.border
                    })
                    .when(over, |d| d.border_dashed())
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_1()
                            .bg(cx.theme().secondary)
                            .text_xs()
                            .when(has_more, |d| {
                                d.cursor_pointer().on_mouse_down(MouseButton::Left, toggle)
                            })
                            .when(has_more, |d| d.child(icon(Icon::chevron(open), p.muted)))
                            .child(icon(Icon::tool(call.kind), p.muted))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .child(call.title.clone()),
                            )
                            .when(added + removed > 0, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_1()
                                        .font_family(mono.clone())
                                        .child(div().text_color(p.success).child(format!("+{added}")))
                                        .child(div().text_color(p.danger).child(format!("−{removed}"))),
                                )
                            })
                            .children(motion::mark(
                                Mark::of_tool(call.status),
                                ("card-mark", ix),
                                color,
                                still,
                            ))
                            .child(badge(call.status.label(self.locale), color)),
                    )
                    .when_some(call.input.clone().filter(|_| open), |d, input| {
                        d.child(
                            div()
                                .px_2()
                                .py_1()
                                .border_t_1()
                                .border_color(p.border)
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(p.muted)
                                .child(input),
                        )
                    })
                    .when(!diff.is_empty(), |d| {
                        let shown = diff.len() - diff_hidden;
                        d.child(
                            div()
                                .id(("tool-diff", ix))
                                .py_1()
                                .border_t_1()
                                .border_color(p.border)
                                .max_h(px(320.))
                                .overflow_y_scroll()
                                .font_family(mono.clone())
                                .text_xs()
                                .children(diff.iter().take(shown).map(|(kind, text)| {
                                    // The marker stays, so the diff reads
                                    // without colour too.
                                    let (mark, color, tint) = match kind {
                                        DiffLineKind::Added => ("+", p.success, 0.12),
                                        DiffLineKind::Removed => ("−", p.danger, 0.12),
                                        DiffLineKind::Context => (" ", p.fg, 0.),
                                        DiffLineKind::Header => (" ", p.muted, 0.),
                                    };
                                    div()
                                        .px_2()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .when(tint > 0., |d| d.bg(color.opacity(tint)))
                                        .text_color(color)
                                        .child(div().flex_none().w(px(8.)).child(mark))
                                        .child(div().flex_1().min_w_0().child(text.to_string()))
                                }))
                                .when(diff_hidden > 0, |d| {
                                    d.child(div().px_2().text_color(p.muted).child(self.trf(
                                        "stream.more_lines",
                                        &[("count", &diff_hidden.to_string())],
                                    )))
                                }),
                        )
                    })
                    .when(!tail.is_empty(), |d| {
                        d.child(
                            div()
                                .id(("tool-output", ix))
                                .px_2()
                                .py_1()
                                .border_t_1()
                                .border_color(p.border)
                                .max_h(px(320.))
                                .overflow_y_scroll()
                                .font_family(mono.clone())
                                .text_xs()
                                .when(hidden > 0, |d| {
                                    d.child(div().text_color(p.muted).child(self.trf(
                                        "stream.earlier_lines",
                                        &[("count", &hidden.to_string())],
                                    )))
                                })
                                .child(tail),
                        )
                    })
                    .into_any_element()
            }
            RowKind::Plan { steps } => div()
                .mx_2()
                .px_2()
                .py_2()
                .rounded(px(6.))
                .border_1()
                .border_color(p.border)
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .text_color(p.muted)
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .child(icon(Icon::Plan, p.muted))
                        .child(self.tr("entry.plan")),
                )
                .children(steps.into_iter().map(|step| {
                    div()
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
                        .child(div().flex_1().text_sm().child(step.text))
                }))
                .into_any_element(),
            RowKind::Check {
                name,
                passed,
                details,
            } => {
                let (check_icon, color) = if passed {
                    (Icon::CheckPass, p.success)
                } else {
                    (Icon::CheckFail, p.danger)
                };
                let foldable = details.is_some();
                div()
                    .px_3()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .text_xs()
                            .when(foldable, |d| {
                                d.cursor_pointer().on_mouse_down(MouseButton::Left, toggle)
                            })
                            .when(foldable, |d| d.child(icon(Icon::chevron(open), p.muted)))
                            .child(icon(check_icon, color))
                            .child(div().flex_1().child(name)),
                    )
                    .when_some(details.filter(|_| open), |d, details| {
                        d.child(
                            div()
                                .ml_1()
                                .pl_3()
                                .border_l_1()
                                .border_color(p.border)
                                .text_xs()
                                .text_color(p.muted)
                                .child(details),
                        )
                    })
                    .into_any_element()
            }
            RowKind::TurnEnd { summary } => div()
                .px_3()
                .text_xs()
                .text_color(p.muted)
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .child(icon(Icon::TurnEnd, p.muted))
                .child(summary)
                .into_any_element(),
            RowKind::Error { title, message, .. } => div()
                .mx_2()
                .px_2()
                .py_2()
                .rounded(px(6.))
                .border_1()
                .border_color(p.danger)
                .bg(p.danger.opacity(0.08))
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .text_sm()
                        .text_color(p.danger)
                        .child(icon(Icon::Error, p.danger))
                        .child(title),
                )
                .child(div().text_xs().child(message))
                .when(self.can_retry(key.seq), |d| {
                    d.child(
                        div().flex().flex_row().child(
                            div()
                                .id(("harness-retry", ix))
                                .px_2()
                                .py_1()
                                .rounded(px(4.))
                                .border_1()
                                .border_color(p.danger.opacity(0.5))
                                .text_xs()
                                .text_color(p.danger)
                                .cursor_pointer()
                                .hover(|d| d.bg(p.danger.opacity(0.12)))
                                .child(self.tr("entry.retry"))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                        this.retry(key.seq);
                                        cx.notify();
                                    }),
                                ),
                        ),
                    )
                })
                .into_any_element(),
        };

        // Every row: its own spacing, and the chip that points at the object
        // its entry is about.
        let after_rewind = self.rewinding.is_some_and(|seq| key.seq > seq);
        let chip: Option<SharedString> = row.anchor.map(Into::into);
        div()
            .w_full()
            .py_1()
            .flex()
            .flex_col()
            .gap_1()
            // What a rewind would drop is shown as already going.
            .when(after_rewind, |d| d.opacity(0.4))
            .child(body)
            .when_some(chip, |d, anchor| {
                let pointing = self.on_anchor.is_some();
                let target = anchor.clone();
                d.child(
                    div().px_3().flex().flex_row().child(
                        div()
                            .id(("harness-anchor", ix))
                            .px_2()
                            .rounded(px(3.))
                            .bg(p.info.opacity(0.16))
                            .text_xs()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_1()
                            // With a product that can show it, the chip is
                            // the way to the object.
                            .when(pointing, |d| {
                                d.cursor_pointer()
                                    .hover(|d| d.bg(p.info.opacity(0.3)))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                            this.show_anchor(&target);
                                            cx.stop_propagation();
                                            cx.notify();
                                        }),
                                    )
                            })
                            .child(icon(Icon::Anchor, p.info))
                            .child(anchor),
                    ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(seq: u64, title: &str) -> Entry {
        Entry::tool(seq, ToolCall::new(ToolKind::Read, title, ToolStatus::Done))
    }

    fn keys(rows: &[Row]) -> Vec<(u64, u32)> {
        rows.iter().map(|row| (row.key.seq, row.key.part)).collect()
    }

    fn paragraphs(count: usize) -> String {
        (0..count)
            .map(|ix| format!("Paragraph {ix} of the answer, some words long.\n\n"))
            .collect()
    }

    #[test]
    fn a_short_answer_is_one_segment() {
        assert_eq!(split_markdown("hello", 2000), vec![0..5]);
        assert_eq!(split_markdown("", 2000), vec![0..0], "an empty answer is still a row");
        let text = "one\n\ntwo\n\nthree\n";
        assert_eq!(split_markdown(text, 2000), vec![0..text.len()]);
    }

    #[test]
    fn a_long_answer_is_cut_at_paragraph_breaks_and_loses_nothing() {
        let text = paragraphs(200);
        let segments = split_markdown(&text, 500);
        assert!(segments.len() > 10, "{}", segments.len());
        let mut end = 0;
        for range in &segments {
            assert_eq!(range.start, end, "segments are contiguous");
            end = range.end;
            let segment = &text[range.clone()];
            assert!(segment.starts_with("Paragraph"), "a cut lands on a block's start");
            assert!(segment.len() >= 500 || range.end == text.len());
            assert!(segment.len() < 600, "and soon after the target");
        }
        assert_eq!(end, text.len());
    }

    #[test]
    fn a_code_block_is_never_cut() {
        let code: String = (0..40).map(|ix| format!("line {ix}\n\n")).collect();
        let text = format!("intro\n\n```rhai\n{code}```\n\noutro paragraph\n\nlast\n");
        let segments = split_markdown(&text, 50);
        let fence_open = text.find("```rhai").unwrap();
        let fence_close = text.rfind("```").unwrap();
        for range in &segments {
            assert!(
                range.start <= fence_open || range.start > fence_close,
                "cut at {} is inside the fence",
                range.start
            );
        }
        // The whole block rides in the first segment; the cut comes after it.
        let outro = text.find("outro").unwrap();
        assert_eq!(segments, vec![0..outro, outro..text.len()]);
    }

    #[test]
    fn cuts_do_not_move_as_an_answer_streams_in() {
        let text = paragraphs(120);
        let whole = split_markdown(&text, 500);
        // Every prefix agrees with the whole on all but its last segment.
        for len in (0..text.len()).step_by(37) {
            let partial = split_markdown(&text[..len], 500);
            let settled = &partial[..partial.len() - 1];
            assert_eq!(settled, &whole[..settled.len()], "at {len} bytes");
        }
    }

    #[test]
    fn entries_become_rows() {
        let stream = rows(&[
            Entry::user(1, "cut holes"),
            Entry::thinking(2, "which face?", Some(900)),
            Entry::assistant(3, "On it."),
            read(4, "Body1"),
            read(5, "TopFace"),
            Entry::tool(
                6,
                ToolCall::new(ToolKind::Search, "holes", ToolStatus::Done),
            ),
            Entry::tool(
                7,
                ToolCall::new(ToolKind::Edit, "pattern_holes", ToolStatus::Running),
            )
            .with_anchor("Body1.TopFace"),
            read(8, "Body1"),
            Entry::check(9, "wall thickness", true),
            Entry::turn_end(10, "12s"),
        ]);
        assert_eq!(
            keys(&stream),
            [(1, 0), (2, 0), (3, 0), (4, 0), (7, 0), (8, 0), (9, 0), (10, 0)],
            "the three reads and searches are one row; the edit is its own"
        );
        let RowKind::Tools(calls) = &stream[3].kind else {
            panic!("expected a run of tool calls");
        };
        assert_eq!(calls.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(), [4, 5, 6]);
        assert!(matches!(stream[4].kind, RowKind::ToolCard(_)));
        assert_eq!(stream[4].anchor.as_deref(), Some("Body1.TopFace"));
        assert!(
            matches!(&stream[5].kind, RowKind::Tools(calls) if calls.len() == 1),
            "a card ends the run before it"
        );
    }

    #[test]
    fn a_long_answer_is_several_rows_and_only_the_last_streams() {
        let entry = Entry::assistant_streaming(5, paragraphs(150)).with_anchor("Body1");
        let stream = rows(&[entry]);
        assert!(stream.len() > 2);
        assert_eq!(stream[0].key, RowKey { seq: 5, part: 0 });
        assert_eq!(stream[1].key, RowKey { seq: 5, part: 1 });
        let streaming: Vec<bool> = stream
            .iter()
            .map(|row| matches!(row.kind, RowKind::Markdown { streaming: true, .. }))
            .collect();
        assert!(streaming[..streaming.len() - 1].iter().all(|s| !s));
        assert!(streaming[streaming.len() - 1]);
        assert_eq!(stream[0].anchor.as_deref(), Some("Body1"));
        assert!(stream[1..].iter().all(|row| row.anchor.is_none()), "one chip per entry");
    }

    #[test]
    fn a_growing_answer_changes_only_its_last_row() {
        let text = paragraphs(150);
        let at = |len: usize| rows(&[Entry::user(1, "q"), Entry::assistant_streaming(2, &text[..len])]);
        let (shorter, longer) = (at(3000), at(3040));
        let change = sync(&shorter, &longer);
        assert_eq!(change.changed, [shorter.len() - 1], "{change:?}");
        assert_eq!(change.splice, None, "no row came or went");

        // Further on a new segment begins: the old last row settles (it is no
        // longer the streaming one) and a row is added after it.
        let much_longer = at(6000);
        let change = sync(&shorter, &much_longer);
        assert_eq!(change.changed, [shorter.len() - 1]);
        let (range, count) = change.splice.expect("rows were added");
        assert_eq!(range, shorter.len()..shorter.len());
        assert_eq!(count, much_longer.len() - shorter.len());
    }

    #[test]
    fn a_call_joining_a_run_changes_that_row() {
        let before = rows(&[Entry::user(1, "q"), read(2, "a")]);
        let after = rows(&[Entry::user(1, "q"), read(2, "a"), read(3, "b")]);
        assert_eq!(
            sync(&before, &after),
            RowSync {
                changed: vec![1],
                splice: None
            }
        );
    }

    #[test]
    fn a_rewind_and_a_switch_splice_the_rows() {
        let long = rows(&(1..=6).map(|seq| Entry::user(seq, "m")).collect::<Vec<_>>());
        let short = rows(&(1..=2).map(|seq| Entry::user(seq, "m")).collect::<Vec<_>>());
        assert_eq!(sync(&long, &short).splice, Some((2..6, 0)));
        assert_eq!(sync(&short, &long).splice, Some((2..2, 4)));
        let other = rows(&[Entry::user(40, "x")]);
        assert_eq!(sync(&long, &other).splice, Some((0..6, 1)));
        assert!(sync(&long, &long).is_empty());
    }

    #[test]
    fn the_stream_state_follows_the_entries_and_forgets_what_left() {
        let mut state = StreamState::default();
        assert!(state.is_following(), "a new stream follows its end");
        let entries: Vec<Entry> = vec![
            Entry::user(1, "q"),
            Entry::thinking(2, "hm", None),
            read(3, "a"),
        ];
        state.set_entries(&entries);
        assert_eq!(state.rows().len(), 3);
        assert_eq!(state.list.item_count(), 3);

        let thinking = RowKey::of(2);
        state.toggle(thinking);
        assert!(state.is_expanded(thinking));
        // Unchanged entries: nothing to do, and what is unfolded stays so.
        assert!(state.set_entries(&entries).is_empty());
        assert!(state.is_expanded(thinking));

        // A rewind past it: the row is gone and so is its unfolded state.
        state.set_entries(&entries[..1]);
        assert_eq!(state.list.item_count(), 1);
        state.set_entries(&entries);
        assert!(!state.is_expanded(thinking), "a row that comes back starts folded");
    }

    #[test]
    fn a_run_is_summed_up_by_kind() {
        let call = |kind, title: &str, status| (0, ToolCall::new(kind, title, status));
        let done = ToolStatus::Done;
        let one = [call(ToolKind::Read, "Body1.TopFace", done)];
        assert_eq!(tools_summary(&one, Locale::En), "Body1.TopFace");
        let run = [
            call(ToolKind::Read, "a", done),
            call(ToolKind::Search, "b", done),
            call(ToolKind::Read, "c", ToolStatus::Running),
            call(ToolKind::Read, "d", done),
        ];
        assert_eq!(tools_summary(&run, Locale::En), "Read ×3 · Search ×1");
        assert_eq!(tools_status(&run), ToolStatus::Running, "one still running");
        assert_eq!(tools_status(&one), ToolStatus::Done);
        let failed = [
            call(ToolKind::Read, "a", done),
            call(ToolKind::Fetch, "b", ToolStatus::Failed),
        ];
        assert_eq!(tools_status(&failed), ToolStatus::Failed);
    }

    #[test]
    fn output_shows_its_last_lines() {
        assert_eq!(output_tail("a\nb\nc", 5), ("a\nb\nc", 0));
        assert_eq!(output_tail("", 5), ("", 0));
        let ten: String = (1..=10).map(|n| format!("line {n}\n")).collect();
        let (tail, hidden) = output_tail(&ten, 3);
        assert_eq!(hidden, 7);
        assert_eq!(tail, "line 8\nline 9\nline 10\n");
    }

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_label(800), "0.8s");
        assert_eq!(duration_label(4_250), "4.2s");
        assert_eq!(duration_label(12_900), "12s");
        assert_eq!(duration_label(125_000), "2m 05s");
    }

    #[test]
    fn a_diff_is_read_line_by_line() {
        let text = "--- a/part\n+++ b/part\n@@ hole pattern @@\n count: 4\n-diameter: 6\n+diameter: 8\n+depth: 12\nplain";
        let (lines, added, removed) = diff_lines(text);
        assert_eq!((added, removed), (2, 1), "headers count as neither");
        use DiffLineKind::*;
        assert_eq!(
            lines,
            [
                (Header, "--- a/part"),
                (Header, "+++ b/part"),
                (Header, "@@ hole pattern @@"),
                (Context, "count: 4"),
                (Removed, "diameter: 6"),
                (Added, "diameter: 8"),
                (Added, "depth: 12"),
                (Context, "plain"),
            ]
        );
        assert_eq!(diff_lines(""), (vec![], 0, 0));
    }

    #[test]
    fn a_call_with_a_diff_is_a_card() {
        let read = ToolCall::new(ToolKind::Read, "read", ToolStatus::Done);
        assert!(!read.is_card());
        assert!(read.with_diff("+x").is_card());
    }

    #[test]
    fn user_messages_are_stepped_through_from_what_is_on_top() {
        let stream = rows(&[
            Entry::user(1, "first"),
            Entry::assistant(2, "a"),
            Entry::user(3, "second"),
            Entry::assistant(4, "b"),
            Entry::user(5, "third"),
        ]);
        assert_eq!(user_message_from(&stream, 3, -1), Some(2));
        assert_eq!(user_message_from(&stream, 2, -1), Some(0), "from one to the one before");
        assert_eq!(user_message_from(&stream, 0, -1), None);
        assert_eq!(user_message_from(&stream, 0, 1), Some(2));
        assert_eq!(user_message_from(&stream, 3, 1), Some(4));
        assert_eq!(user_message_from(&stream, 4, 1), None);
        assert_eq!(user_message_from(&stream, 99, -1), Some(4), "past the end");
        assert_eq!(user_message_from(&[], 0, 1), None);
    }

    #[test]
    fn all_tool_rows_unfold_and_fold_together() {
        let mut state = StreamState::default();
        assert!(!state.toggle_tools(), "no tool rows");
        state.set_entries(&[
            Entry::user(1, "q"),
            Entry::tool(2, ToolCall::new(ToolKind::Edit, "edit", ToolStatus::Done).with_output("x")),
            Entry::assistant(3, "a"),
            Entry::tool(4, ToolCall::new(ToolKind::Read, "read", ToolStatus::Done)),
        ]);
        let tools: Vec<RowKey> = state
            .rows()
            .iter()
            .filter(|row| matches!(row.kind, RowKind::Tools(_) | RowKind::ToolCard(_)))
            .map(|row| row.key)
            .collect();
        assert_eq!(tools.len(), 2);

        // One is open already: the rest follow, then all close.
        state.toggle(tools[0]);
        assert!(state.toggle_tools());
        assert!(tools.iter().all(|key| state.is_expanded(*key)));
        assert!(state.toggle_tools());
        assert!(tools.iter().all(|key| !state.is_expanded(*key)));
    }
}
