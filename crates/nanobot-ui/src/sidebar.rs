//! What the session sidebar lists, and in what order
//! (docs/ui-redesign-design.md §4).
//!
//! Every project is a group; its sessions are rows under it. The rows are
//! computed here from the store, as plain data, and tested without a window;
//! the second half of this file draws them.
//!
//! - A project folds. Folded, its heading still says how many of its sessions
//!   are waiting for the user — otherwise folding it would hide that.
//! - Within a project, sessions waiting for the user come first, then the ones
//!   running, then the rest, most recently active first.
//! - Sessions idle for more than [`INACTIVE_AFTER_MS`] gather under one
//!   "earlier" row, folded until asked for.
//! - Archived sessions are not listed.
//! - A filter keeps the sessions whose title contains it and shows every
//!   project that still has one, unfolded.

use std::collections::HashSet;

use gpui::{
    div, prelude::*, px, AnyElement, Context, Entity, Hsla, MouseButton, MouseDownEvent,
    Subscription, Window,
};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::StyledExt as _;

use crate::i18n::Locale;
use crate::icons::{icon, Icon};
use crate::shell::{palette, HarnessShell, Palette};

use crate::model::{
    plan_progress, resolve_sidebar_groups, ProjectId, SessionId, SessionStore, SessionView,
    SidebarProjection,
};
use crate::transcript::SessionBadge;

/// A session untouched for this long is "earlier": 72 hours.
pub const INACTIVE_AFTER_MS: u64 = 72 * 60 * 60 * 1000;

/// One row of the sidebar.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SidebarRow {
    /// A project's heading.
    Project {
        id: ProjectId,
        name: String,
        /// The project whose sessions the shell can open directly.
        open: bool,
        expanded: bool,
        /// Sessions listed under it (archived ones excluded).
        sessions: usize,
        /// How many of them want the user.
        waiting: usize,
        running: usize,
    },
    /// The heading of one of the product's own groups inside the open project.
    Group { label: String },
    Session {
        project: ProjectId,
        id: SessionId,
        title: String,
        badge: SessionBadge,
        /// How long since it was last active ("5m", "3h", "2d"), when known.
        age: Option<String>,
        /// It was forked from another session.
        forked: bool,
        /// Steps done and steps in all, of its plan.
        plan: (usize, usize),
        /// It is the session on screen.
        active: bool,
    },
    /// The fold that holds a project's long-idle sessions.
    Earlier {
        project: ProjectId,
        count: usize,
        expanded: bool,
    },
}

impl SidebarRow {
    /// Whether the keyboard can rest on the row.
    pub fn is_selectable(&self) -> bool {
        !matches!(self, Self::Group { .. })
    }
}

/// What the rows are computed from.
pub struct SidebarInput<'a, S: SessionView> {
    pub store: &'a SessionStore<S>,
    pub projection: Option<&'a dyn SidebarProjection<S>>,
    /// What is typed in the filter box.
    pub filter: &'a str,
    /// Projects whose sessions show.
    pub expanded: &'a HashSet<ProjectId>,
    /// Projects whose "earlier" fold is open.
    pub earlier_expanded: &'a HashSet<ProjectId>,
    /// Sessions that finished a turn out of sight.
    pub unread: &'a HashSet<SessionId>,
    /// The session on screen.
    pub active: Option<&'a SessionId>,
    pub now_ms: u64,
    /// The language ages are written in.
    pub locale: Locale,
}

/// How long ago `then_ms` was, as briefly as it reads: "now", "5m", "3h",
/// "2d", "6w".
pub fn age_label(locale: Locale, now_ms: u64, then_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms) / 60_000;
    let (key, count) = match minutes {
        0 => return crate::i18n::t(locale, "age.now").to_string(),
        1..=59 => ("age.minutes", minutes),
        60..=1439 => ("age.hours", minutes / 60),
        1440..=20_159 => ("age.days", minutes / 1440),
        _ => ("age.weeks", minutes / 10_080),
    };
    crate::i18n::t_replace(locale, key, &[("count", &count.to_string())])
}


/// Where a badge puts a session in its project: wanting the user, running,
/// everything else.
fn urgency(badge: SessionBadge) -> u8 {
    match badge {
        SessionBadge::AwaitingUser | SessionBadge::Error => 0,
        SessionBadge::Running => 1,
        SessionBadge::Unread | SessionBadge::None => 2,
    }
}

/// Most pressing first, then most recently active; ties keep the given order.
fn ordered<S: SessionView>(
    mut sessions: Vec<&S>,
    badge_of: impl Fn(&S) -> SessionBadge,
) -> Vec<&S> {
    sessions.sort_by_key(|session| {
        (
            urgency(badge_of(session)),
            std::cmp::Reverse(session.last_active_ms().unwrap_or(0)),
        )
    });
    sessions
}

/// The sidebar's rows, top to bottom.
pub fn rows<S: SessionView>(input: &SidebarInput<'_, S>) -> Vec<SidebarRow> {
    let filter = input.filter.trim().to_lowercase();
    let filtering = !filter.is_empty();
    let open_id = input.store.open_project_id();
    let mut rows = Vec::new();

    for project in input.store.projects() {
        let is_open = &project.id == open_id;
        let badge_of = |session: &S| {
            SessionBadge::of(session.status(), input.unread.contains(session.id()))
        };
        let listed: Vec<&S> = project
            .sessions
            .iter()
            .filter(|session| !session.archived())
            .filter(|session| !filtering || session.title().to_lowercase().contains(&filter))
            .collect();
        if filtering && listed.is_empty() {
            continue;
        }
        let expanded = filtering || input.expanded.contains(&project.id);
        rows.push(SidebarRow::Project {
            id: project.id.clone(),
            name: project.name.clone(),
            open: is_open,
            expanded,
            sessions: listed.len(),
            waiting: listed
                .iter()
                .filter(|session| badge_of(session).needs_attention())
                .count(),
            running: listed
                .iter()
                .filter(|session| badge_of(session) == SessionBadge::Running)
                .count(),
        });
        if !expanded {
            continue;
        }

        let row_of = |session: &S| SidebarRow::Session {
            project: project.id.clone(),
            id: session.id().clone(),
            title: session.title(),
            badge: badge_of(session),
            age: session
                .last_active_ms()
                .map(|then| age_label(input.locale, input.now_ms, then)),
            forked: session.branch_marker().is_some(),
            plan: plan_progress(&session.plan()),
            active: input.active == Some(session.id()),
        };
        // Long idle, nothing to say, and not what the user is looking at.
        let is_earlier = |session: &S| {
            !filtering
                && input.active != Some(session.id())
                && badge_of(session) == SessionBadge::None
                && session
                    .last_active_ms()
                    .is_some_and(|then| input.now_ms.saturating_sub(then) > INACTIVE_AFTER_MS)
        };
        // The open project may be arranged by the product (its own groups);
        // every other project is a flat list.
        let groups: Vec<(Option<String>, Vec<&S>)> = match (is_open, input.projection) {
            (true, Some(projection)) => {
                resolve_sidebar_groups(input.store, Some(projection))
                    .into_iter()
                    .map(|(label, members)| {
                        // In the product's order, of what is listed at all.
                        let kept = members
                            .iter()
                            .filter_map(|member| {
                                listed.iter().copied().find(|s| s.id() == member.id())
                            })
                            .collect();
                        (Some(label), kept)
                    })
                    .collect()
            }
            _ => vec![(None, listed.clone())],
        };

        let mut earlier: Vec<&S> = Vec::new();
        for (label, members) in groups {
            let (old, current): (Vec<&S>, Vec<&S>) =
                members.into_iter().partition(|session| is_earlier(session));
            earlier.extend(old);
            if current.is_empty() {
                continue;
            }
            if let Some(label) = label {
                rows.push(SidebarRow::Group { label });
            }
            rows.extend(ordered(current, badge_of).into_iter().map(row_of));
        }
        if !earlier.is_empty() {
            let unfolded = input.earlier_expanded.contains(&project.id);
            rows.push(SidebarRow::Earlier {
                project: project.id.clone(),
                count: earlier.len(),
                expanded: unfolded,
            });
            if unfolded {
                rows.extend(ordered(earlier, badge_of).into_iter().map(row_of));
            }
        }
    }
    rows
}

/// The row the keyboard moves to from `current` by `delta` (`+1` down, `-1`
/// up), skipping headings it cannot rest on. It stops at the ends.
pub fn step(rows: &[SidebarRow], current: Option<usize>, delta: isize) -> Option<usize> {
    let selectable: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.is_selectable())
        .map(|(ix, _)| ix)
        .collect();
    let last = selectable.len().checked_sub(1)?;
    let at = match current.and_then(|ix| selectable.iter().position(|row| *row == ix)) {
        Some(at) => (at as isize + delta).clamp(0, last as isize) as usize,
        // Nothing selected yet: down starts at the top, up at the bottom.
        None if delta >= 0 => 0,
        None => last,
    };
    Some(selectable[at])
}

/// The first session that wants the user, for the attention badge to land on.
pub fn first_waiting(rows: &[SidebarRow]) -> Option<usize> {
    rows.iter().position(|row| {
        matches!(row, SidebarRow::Session { badge, .. } if badge.needs_attention())
    })
}

// ---------------------------------------------------------------------------
// What the shell does with the rows. Runs without a window.

/// A session's title being edited in place.
pub(crate) struct Renaming {
    pub(crate) id: SessionId,
    pub(crate) input: Entity<InputState>,
    _subscription: Subscription,
}

impl<S: SessionView> HarnessShell<S> {
    /// The sidebar's rows now.
    pub fn sidebar_rows(&self) -> Vec<SidebarRow> {
        let store = self.store();
        rows(&SidebarInput {
            store: &store,
            projection: self.sidebar_projection.as_deref(),
            filter: &self.sidebar_query,
            expanded: &self.expanded_projects,
            earlier_expanded: &self.earlier_expanded,
            unread: &self.unread.borrow(),
            active: self.active_session_id(),
            now_ms: nanobot_boot::account::now_ms(),
            locale: self.locale,
        })
    }

    /// What is typed in the sidebar's filter.
    pub fn set_sidebar_filter(&mut self, filter: impl Into<String>) {
        self.sidebar_query = filter.into();
        // The rows under the selection changed.
        self.sidebar_selected = None;
    }

    /// Folds or unfolds a project's sessions.
    pub fn toggle_project(&mut self, project: &ProjectId) {
        if !self.expanded_projects.remove(project) {
            self.expanded_projects.insert(project.clone());
        }
    }

    /// Folds or unfolds a project's long-idle sessions.
    pub fn toggle_earlier(&mut self, project: &ProjectId) {
        if !self.earlier_expanded.remove(project) {
            self.earlier_expanded.insert(project.clone());
        }
    }

    /// Puts `session` of `project` on screen, opening the project first when
    /// it is not the open one. False when there is no such session, or it is
    /// on screen already.
    pub fn open_session(&mut self, project: &ProjectId, session: &SessionId) -> bool {
        let known = self
            .store()
            .project(project)
            .is_some_and(|project| project.session(session).is_some());
        if !known {
            return false;
        }
        let switched = self.store().open_project_id() != project && self.switch_project(project);
        self.expanded_projects.insert(project.clone());
        self.switch_session(session) || switched
    }

    /// The row the keyboard is on.
    pub fn sidebar_selection(&self) -> Option<usize> {
        self.sidebar_selected
    }

    /// Moves the keyboard's row down (`+1`) or up (`-1`).
    pub fn step_sidebar(&mut self, delta: isize) {
        self.sidebar_selected = step(&self.sidebar_rows(), self.sidebar_selected, delta);
    }

    /// Puts the keyboard on the first session that wants the user: where the
    /// toolbar's count takes it.
    pub fn select_first_waiting(&mut self) {
        if let Some(row) = first_waiting(&self.sidebar_rows()) {
            self.sidebar_selected = Some(row);
        }
    }

    /// Does what Enter does on the keyboard's row: opens a session, folds or
    /// unfolds a heading. False with no row selected.
    pub fn activate_sidebar_selection(&mut self) -> bool {
        let rows = self.sidebar_rows();
        match self.sidebar_selected.and_then(|ix| rows.get(ix)) {
            Some(SidebarRow::Session { project, id, .. }) => {
                self.open_session(project, id);
            }
            Some(SidebarRow::Project { id, .. }) => self.toggle_project(id),
            Some(SidebarRow::Earlier { project, .. }) => self.toggle_earlier(project),
            Some(SidebarRow::Group { .. }) | None => return false,
        }
        true
    }

    /// Folds (`false`) or unfolds (`true`) the heading the keyboard is on:
    /// the left and right arrows.
    pub fn set_sidebar_selection_expanded(&mut self, expand: bool) {
        let rows = self.sidebar_rows();
        match self.sidebar_selected.and_then(|ix| rows.get(ix)) {
            Some(SidebarRow::Project { id, expanded, .. }) if *expanded != expand => {
                self.toggle_project(id);
            }
            Some(SidebarRow::Earlier {
                project, expanded, ..
            }) if *expanded != expand => self.toggle_earlier(project),
            _ => {}
        }
    }

    /// The session the keyboard is on, if it is on one.
    pub fn selected_session(&self) -> Option<SessionId> {
        match self.sidebar_rows().get(self.sidebar_selected?) {
            Some(SidebarRow::Session { id, .. }) => Some(id.clone()),
            _ => None,
        }
    }

    /// What renaming a session does: the session and its new title.
    pub fn set_on_rename<F: Fn(&SessionId, &str) + 'static>(&mut self, callback: F) {
        self.on_rename = Some(std::rc::Rc::new(callback));
    }

    /// What archiving a session does.
    pub fn set_on_archive<F: Fn(&SessionId) + 'static>(&mut self, callback: F) {
        self.on_archive = Some(std::rc::Rc::new(callback));
    }

    /// Asks the product to rename `session`. False for an empty title or a
    /// product that gave no way to rename.
    pub fn rename_session(&mut self, session: &SessionId, title: &str) -> bool {
        let title = title.trim();
        match (self.on_rename.clone(), title.is_empty()) {
            (Some(callback), false) => {
                callback(session, title);
                true
            }
            _ => false,
        }
    }

    /// Asks the product to archive `session`.
    pub fn archive_session(&mut self, session: &SessionId) -> bool {
        match self.on_archive.clone() {
            Some(callback) => {
                callback(session);
                self.sidebar_selected = None;
                true
            }
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// With a window.

fn badge_color(badge: SessionBadge, p: Palette) -> Option<Hsla> {
    match badge {
        SessionBadge::AwaitingUser => Some(p.warning),
        SessionBadge::Error => Some(p.danger),
        SessionBadge::Running => Some(p.info),
        SessionBadge::Unread => Some(p.accent),
        SessionBadge::None => None,
    }
}

impl<S: SessionView> HarnessShell<S> {
    /// Starts editing the title of `session` in its row.
    pub(crate) fn begin_rename_in(
        &mut self,
        session: SessionId,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.on_rename.is_none() {
            return;
        }
        let input = cx.new(|cx| InputState::new(window, cx).default_value(title.to_string()));
        let subscription = cx.subscribe_in(
            &input,
            window,
            |shell: &mut Self, input, event: &InputEvent, _window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let title = input.read(cx).value().to_string();
                    if let Some(renaming) = shell.renaming.take() {
                        shell.rename_session(&renaming.id, &title);
                    }
                    cx.notify();
                }
                // Clicking away gives the edit up.
                InputEvent::Blur => {
                    shell.renaming = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.renaming = Some(Renaming {
            id: session,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    /// The sidebar: the filter and the new-session button, the rows, and the
    /// account at the foot.
    pub(crate) fn render_sidebar(&self, width: f32, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let rows = self.sidebar_rows();
        let selected = self.sidebar_selected;
        let list = div()
            .id("harness-session-list")
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .children(
                rows.iter()
                    .enumerate()
                    .map(|(ix, row)| self.sidebar_row(ix, row, selected == Some(ix), p, cx)),
            )
            .when(rows.is_empty(), |d| {
                d.child(
                    div()
                        .p_3()
                        .text_xs()
                        .text_color(p.muted)
                        .child(self.tr("sidebar.nothing")),
                )
            });

        div()
            .w(px(width))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .key_context(crate::shell::SIDEBAR_CONTEXT)
            .when_some(self.sidebar_focus.clone(), |d, focus| d.track_focus(&focus))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarUp, _w, cx| {
                this.step_sidebar(-1);
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarDown, _w, cx| {
                this.step_sidebar(1);
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarOpen, _w, cx| {
                this.activate_sidebar_selection();
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarFold, _w, cx| {
                this.set_sidebar_selection_expanded(false);
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarUnfold, _w, cx| {
                this.set_sidebar_selection_expanded(true);
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarRename, window, cx| {
                let rows = this.sidebar_rows();
                let target = this.sidebar_selection().and_then(|ix| rows.get(ix)).cloned();
                if let Some(SidebarRow::Session { id, title, .. }) = target {
                    this.begin_rename_in(id, &title, window, cx);
                }
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarArchive, _w, cx| {
                if let Some(session) = this.selected_session() {
                    this.archive_session(&session);
                }
                cx.notify();
            }))
            .on_action(cx.listener(|this: &mut Self, _: &crate::shell::SidebarFilter, window, cx| {
                if let Some(filter) = this.sidebar_filter.clone() {
                    filter.update(cx, |state, cx| state.focus(window, cx));
                }
            }))
            .child(
                div()
                    .flex_none()
                    .p_2()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .border_b_1()
                    .border_color(p.border)
                    .when_some(self.sidebar_filter.clone(), |d, filter| {
                        d.child(div().flex_1().min_w_0().child(Input::new(&filter).w_full()))
                    })
                    .when(self.sidebar_filter.is_none(), |d| d.child(div().flex_1()))
                    .when(self.on_new_session.is_some(), |d| {
                        d.child(
                            div()
                                .id("harness-sidebar-new")
                                .p_1()
                                .rounded(px(4.))
                                .cursor_pointer()
                                .child(icon(Icon::Plus, p.muted))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                                        this.run_command_in(crate::palette::id::NEW_SESSION, window, cx);
                                    }),
                                ),
                        )
                    }),
            )
            .child(list)
            .child(self.sidebar_footer(p, cx))
    }

    fn sidebar_row(
        &self,
        ix: usize,
        row: &SidebarRow,
        selected: bool,
        p: Palette,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // Where the keyboard is: an outline, so it reads apart from the
        // session on screen (a fill).
        let outline = if selected { p.accent } else { gpui::transparent_black() };
        let still = self.still(cx);
        match row {
            SidebarRow::Project {
                id,
                name,
                open,
                expanded,
                sessions,
                waiting,
                running,
            } => {
                let project = id.clone();
                div()
                    .id(("harness-project", ix))
                    .mx_1()
                    .px_1()
                    .py_1()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(outline)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            this.sidebar_selected = Some(ix);
                            this.toggle_project(&project);
                            cx.notify();
                        }),
                    )
                    .child(icon(Icon::chevron(*expanded), p.muted))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .when(*open, |d| d.font_semibold())
                            .child(name.clone()),
                    )
                    .when(*running > 0, |d| {
                        d.child(div().text_xs().text_color(p.info).child(running.to_string()))
                    })
                    .when(*waiting > 0, |d| {
                        d.child(
                            div()
                                .px_1()
                                .rounded(px(8.))
                                .bg(p.warning.opacity(0.18))
                                .text_xs()
                                .text_color(p.warning)
                                .child(waiting.to_string()),
                        )
                    })
                    .child(div().text_xs().text_color(p.muted).child(sessions.to_string()))
                    .into_any_element()
            }
            SidebarRow::Group { label } => div()
                .px_3()
                .pt_1()
                .text_xs()
                .text_color(p.muted)
                .child(label.clone())
                .into_any_element(),
            SidebarRow::Earlier {
                project,
                count,
                expanded,
            } => {
                let project = project.clone();
                div()
                    .id(("harness-earlier", ix))
                    .mx_1()
                    .pl_4()
                    .pr_1()
                    .py_1()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(outline)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .text_color(p.muted)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            this.sidebar_selected = Some(ix);
                            this.toggle_earlier(&project);
                            cx.notify();
                        }),
                    )
                    .child(icon(Icon::chevron(*expanded), p.muted))
                    .child(self.trf("sidebar.earlier", &[("count", &count.to_string())]))
                    .into_any_element()
            }
            SidebarRow::Session {
                project,
                id,
                title,
                badge,
                age,
                forked,
                plan,
                active,
            } => {
                let (project, session) = (project.clone(), id.clone());
                let renaming = self
                    .renaming
                    .as_ref()
                    .filter(|renaming| &renaming.id == id)
                    .map(|renaming| renaming.input.clone());
                let dot = badge_color(*badge, p);
                // What the session is doing, in a box of one size: running
                // turns, waiting breathes, the rest is a plain dot.
                let plain_dot = |color: Hsla| div().size(px(8.)).rounded(px(4.)).bg(color);
                let status_mark = match (*badge, dot) {
                    (SessionBadge::Running, Some(color)) => {
                        Some(crate::motion::spinner(("session-spin", ix), 10., color, still))
                    }
                    (SessionBadge::AwaitingUser, Some(color)) => {
                        Some(crate::motion::pulse(("session-wait", ix), plain_dot(color), still))
                    }
                    (_, Some(color)) => Some(plain_dot(color).into_any_element()),
                    (_, None) => None,
                };
                let group = gpui::SharedString::from(format!("harness-session-{ix}"));
                let archive = self.on_archive.is_some().then(|| {
                    let session = id.clone();
                    div()
                        .id(("harness-archive", ix))
                        .p_1()
                        .rounded(px(4.))
                        .cursor_pointer()
                        // Shown while the pointer is on the row.
                        .invisible()
                        .group_hover(group.clone(), |style| style.visible())
                        .child(icon(Icon::Archive, p.muted))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                                this.archive_session(&session);
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        )
                });
                div()
                    .id(("harness-session", ix))
                    .group(group.clone())
                    .mx_1()
                    .pl_4()
                    .pr_1()
                    .py_1()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(outline)
                    .when(*active, |d| d.bg(p.accent.opacity(0.22)))
                    .flex()
                    .flex_col()
                    // The right button asks what can be done with it.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener({
                            let (session, title) = (id.clone(), title.clone());
                            move |this: &mut Self, ev: &MouseDownEvent, _w, cx| {
                                this.sidebar_selected = Some(ix);
                                this.open_session_menu(
                                    session.clone(),
                                    &title,
                                    (f32::from(ev.position.x), f32::from(ev.position.y)),
                                );
                                cx.notify();
                            }
                        }),
                    )
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, _w, cx| {
                            this.sidebar_selected = Some(ix);
                            this.open_session(&project, &session);
                            cx.notify();
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .size(px(10.))
                                    .flex_none()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .children(status_mark),
                            )
                            .child(match renaming {
                                Some(input) => div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(Input::new(&input).w_full())
                                    .into_any_element(),
                                None => div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .when(*active, |d| d.font_semibold())
                                    .child(title.clone())
                                    .into_any_element(),
                            })
                            .when_some(age.clone(), |d, age| {
                                d.child(div().text_xs().text_color(p.muted).child(age))
                            })
                            .children(archive),
                    )
                    // A second line only for what is worth one.
                    .when(*forked || plan.1 > 0, |d| {
                        d.child(
                            div()
                                .pl_4()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_xs()
                                .text_color(p.muted)
                                .when(*forked, |d| d.child(icon(Icon::Branch, p.muted)))
                                .when(plan.1 > 0, |d| {
                                    d.child(icon(Icon::Plan, p.muted))
                                        .child(format!("{}/{}", plan.0, plan.1))
                                }),
                        )
                    })
                    .into_any_element()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        BranchMarker, PlanPhase, PlanStep, Project, SessionStatus, SidebarGroup, WorkspaceView,
    };
    use crate::transcript::Conversation;
    use nanobot_gpu::{ViewportProjection, ViewportSlot};

    const HOUR: u64 = 60 * 60 * 1000;
    const NOW: u64 = 1_000 * HOUR;

    #[derive(Clone)]
    struct Item {
        id: SessionId,
        title: String,
        status: SessionStatus,
        archived: bool,
        last_active: Option<u64>,
        forked: bool,
    }

    fn item(id: &str) -> Item {
        Item {
            id: SessionId::new(id),
            title: format!("session {id}"),
            status: SessionStatus::Idle,
            archived: false,
            last_active: None,
            forked: false,
        }
    }

    struct NoViewport;

    impl ViewportSlot for NoViewport {
        type Handle = ();
        type Scene = ();
        fn new_handle() {}
        fn update_scene(_: &(), _: &()) {}
        fn projection(_: &()) -> Option<ViewportProjection> {
            None
        }
        fn painted_frames(_: &()) -> u64 {
            0
        }
        fn element(_: &()) -> gpui::AnyElement {
            gpui::IntoElement::into_any_element(gpui::div())
        }
    }

    impl WorkspaceView for Item {
        fn session_id(&self) -> &SessionId {
            &self.id
        }
        fn title(&self) -> String {
            self.title.clone()
        }
        fn status(&self) -> SessionStatus {
            self.status
        }
        fn conversation(&self) -> Conversation {
            Conversation::default()
        }
        fn plan(&self) -> Vec<PlanStep> {
            Vec::new()
        }
        fn plan_phase(&self) -> PlanPhase {
            PlanPhase::None
        }
        fn clarify_questions(&self) -> Vec<String> {
            Vec::new()
        }
        fn progress(&self) -> Option<(u32, u32)> {
            None
        }
    }

    impl SessionView for Item {
        type Scene = ();
        type Viewport = NoViewport;
        type Workspace = Item;
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
            Conversation::default()
        }
        fn plan(&self) -> Vec<PlanStep> {
            Vec::new()
        }
        fn plan_phase(&self) -> PlanPhase {
            PlanPhase::None
        }
        fn clarify_questions(&self) -> Vec<String> {
            Vec::new()
        }
        fn branch_marker(&self) -> Option<BranchMarker> {
            self.forked.then(|| BranchMarker {
                parent_session: SessionId::new("parent"),
                parent_seq: 1,
            })
        }
        fn archived(&self) -> bool {
            self.archived
        }
        fn last_active_ms(&self) -> Option<u64> {
            self.last_active
        }
        fn build_workspace(&self, _: ()) -> Item {
            self.clone()
        }
    }

    fn store(projects: Vec<(&str, Vec<Item>)>) -> SessionStore<Item> {
        let open = ProjectId::new(projects[0].0);
        SessionStore::new(
            projects
                .into_iter()
                .map(|(id, sessions)| Project {
                    id: ProjectId::new(id),
                    name: format!("project {id}"),
                    session_index: sessions.first().map(|s| s.id.clone()),
                    sessions,
                })
                .collect(),
            open,
        )
    }

    /// The rows for `store` with every project unfolded and nothing else set.
    struct View {
        expanded: HashSet<ProjectId>,
        earlier: HashSet<ProjectId>,
        unread: HashSet<SessionId>,
        filter: String,
        active: Option<SessionId>,
    }

    impl View {
        fn all(store: &SessionStore<Item>) -> Self {
            Self {
                expanded: store.projects().iter().map(|p| p.id.clone()).collect(),
                earlier: HashSet::new(),
                unread: HashSet::new(),
                filter: String::new(),
                active: None,
            }
        }

        fn rows(
            &self,
            store: &SessionStore<Item>,
            projection: Option<&dyn SidebarProjection<Item>>,
        ) -> Vec<SidebarRow> {
            rows(&SidebarInput {
                store,
                projection,
                filter: &self.filter,
                expanded: &self.expanded,
                earlier_expanded: &self.earlier,
                unread: &self.unread,
                active: self.active.as_ref(),
                now_ms: NOW,
                locale: Locale::En,
            })
        }
    }

    /// The rows as short words: "P:p1", "S:s1", "G:label", "E:3".
    fn brief(rows: &[SidebarRow]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                SidebarRow::Project { id, .. } => format!("P:{id}"),
                SidebarRow::Group { label } => format!("G:{label}"),
                SidebarRow::Session { id, .. } => format!("S:{id}"),
                SidebarRow::Earlier { count, .. } => format!("E:{count}"),
            })
            .collect()
    }

    #[test]
    fn every_project_is_a_group_and_folds() {
        let store = store(vec![
            ("p1", vec![item("a"), item("b")]),
            ("p2", vec![item("c")]),
        ]);
        let mut view = View::all(&store);
        assert_eq!(brief(&view.rows(&store, None)), ["P:p1", "S:a", "S:b", "P:p2", "S:c"]);

        view.expanded.remove(&ProjectId::new("p1"));
        let rows = view.rows(&store, None);
        assert_eq!(brief(&rows), ["P:p1", "P:p2", "S:c"]);
        assert!(matches!(
            &rows[0],
            SidebarRow::Project { open: true, expanded: false, sessions: 2, .. }
        ));
        assert!(matches!(&rows[1], SidebarRow::Project { open: false, expanded: true, .. }));
    }

    #[test]
    fn a_folded_project_still_says_who_is_waiting() {
        let waiting = Item {
            status: SessionStatus::AwaitingUser,
            ..item("b")
        };
        let failed = Item {
            status: SessionStatus::Error,
            ..item("c")
        };
        let running = Item {
            status: SessionStatus::Running,
            ..item("d")
        };
        let store = store(vec![("p1", vec![item("a"), waiting, failed, running])]);
        let mut view = View::all(&store);
        view.expanded.clear();
        let rows = view.rows(&store, None);
        assert_eq!(
            rows,
            [SidebarRow::Project {
                id: ProjectId::new("p1"),
                name: "project p1".to_string(),
                open: true,
                expanded: false,
                sessions: 4,
                waiting: 2,
                running: 1,
            }]
        );
    }

    #[test]
    fn sessions_are_ordered_by_what_they_want_then_by_recency() {
        let at = |id: &str, status, hours_ago: u64| Item {
            status,
            last_active: Some(NOW - hours_ago * HOUR),
            ..item(id)
        };
        let store = store(vec![(
            "p1",
            vec![
                at("old", SessionStatus::Idle, 5),
                at("running", SessionStatus::Running, 9),
                at("new", SessionStatus::Idle, 1),
                at("waiting", SessionStatus::AwaitingUser, 20),
                at("failed", SessionStatus::Error, 2),
            ],
        )]);
        let rows = View::all(&store).rows(&store, None);
        assert_eq!(
            brief(&rows),
            ["P:p1", "S:failed", "S:waiting", "S:running", "S:new", "S:old"]
        );
        assert!(matches!(&rows[4], SidebarRow::Session { age: Some(age), .. } if age == "1h"));
    }

    #[test]
    fn long_idle_sessions_gather_under_earlier() {
        let at = |id: &str, hours_ago: u64| Item {
            last_active: Some(NOW - hours_ago * HOUR),
            ..item(id)
        };
        let store = store(vec![(
            "p1",
            vec![
                at("today", 3),
                at("last-week", 200),
                at("last-month", 800),
                // Old, but wants the user: it stays where it is seen.
                Item {
                    status: SessionStatus::AwaitingUser,
                    ..at("old-waiting", 500)
                },
                // No time known: it cannot be called old.
                item("timeless"),
            ],
        )]);
        let mut view = View::all(&store);
        assert_eq!(
            brief(&view.rows(&store, None)),
            ["P:p1", "S:old-waiting", "S:today", "S:timeless", "E:2"]
        );

        view.earlier.insert(ProjectId::new("p1"));
        assert_eq!(
            brief(&view.rows(&store, None)),
            ["P:p1", "S:old-waiting", "S:today", "S:timeless", "E:2", "S:last-week", "S:last-month"]
        );

        // The session on screen is never folded away, however old.
        view.earlier.clear();
        view.active = Some(SessionId::new("last-month"));
        assert_eq!(
            brief(&view.rows(&store, None)),
            ["P:p1", "S:old-waiting", "S:today", "S:last-month", "S:timeless", "E:1"]
        );
    }

    #[test]
    fn archived_sessions_are_not_listed_and_unread_ones_are_marked() {
        let store = store(vec![(
            "p1",
            vec![
                item("a"),
                Item {
                    archived: true,
                    ..item("gone")
                },
                Item {
                    forked: true,
                    ..item("b")
                },
            ],
        )]);
        let mut view = View::all(&store);
        view.unread.insert(SessionId::new("b"));
        view.active = Some(SessionId::new("a"));
        let rows = view.rows(&store, None);
        assert_eq!(brief(&rows), ["P:p1", "S:a", "S:b"]);
        assert!(matches!(&rows[0], SidebarRow::Project { sessions: 2, .. }));
        assert!(matches!(
            &rows[1],
            SidebarRow::Session { active: true, badge: SessionBadge::None, forked: false, .. }
        ));
        assert!(matches!(
            &rows[2],
            SidebarRow::Session { active: false, badge: SessionBadge::Unread, forked: true, .. }
        ));
    }

    #[test]
    fn a_filter_keeps_matching_sessions_and_unfolds_their_projects() {
        let named = |id: &str, title: &str| Item {
            title: title.to_string(),
            last_active: Some(NOW - 900 * HOUR),
            ..item(id)
        };
        let store = store(vec![
            ("p1", vec![named("a", "Bracket holes"), named("b", "Flange draft")]),
            ("p2", vec![named("c", "Housing shell")]),
            ("p3", vec![named("d", "bracket chamfer")]),
        ]);
        let mut view = View::all(&store);
        view.expanded.clear();
        view.filter = "  BRACKET ".to_string();
        // Folded projects open, the "earlier" fold does not apply, and a
        // project with nothing left is not shown.
        assert_eq!(brief(&view.rows(&store, None)), ["P:p1", "S:a", "P:p3", "S:d"]);
        view.filter = "nothing like it".to_string();
        assert!(view.rows(&store, None).is_empty());
    }

    struct ByLetter;

    impl SidebarProjection<Item> for ByLetter {
        fn groups(&self, _store: &SessionStore<Item>) -> Vec<SidebarGroup> {
            vec![
                SidebarGroup::new("Design A", vec![SessionId::new("a2"), SessionId::new("a1")]),
                SidebarGroup::new("Design B", vec![SessionId::new("b1")]),
            ]
        }
    }

    #[test]
    fn the_open_project_takes_the_products_groups() {
        let store = store(vec![
            ("p1", vec![item("a1"), item("b1"), item("a2"), item("loose")]),
            ("p2", vec![item("c")]),
        ]);
        let rows = View::all(&store).rows(&store, Some(&ByLetter));
        assert_eq!(
            brief(&rows),
            [
                "P:p1", "G:Design A", "S:a2", "S:a1", "G:Design B", "S:b1", "G:其他", "S:loose",
                "P:p2", "S:c"
            ],
            "sessions the product did not name stay reachable; other projects stay flat"
        );
    }

    #[test]
    fn the_keyboard_skips_headings_and_stops_at_the_ends() {
        let store = store(vec![("p1", vec![item("a1"), item("b1")])]);
        let rows = View::all(&store).rows(&store, Some(&ByLetter));
        // P:p1, G:Design A, S:a1, G:Design B, S:b1 (a2 is not in this store)
        assert_eq!(step(&rows, None, 1), Some(0), "down from nowhere is the top");
        assert_eq!(step(&rows, None, -1), Some(4), "up from nowhere is the bottom");
        assert_eq!(step(&rows, Some(0), 1), Some(2), "over the group heading");
        assert_eq!(step(&rows, Some(2), 1), Some(4));
        assert_eq!(step(&rows, Some(4), 1), Some(4), "it stops");
        assert_eq!(step(&rows, Some(2), -1), Some(0));
        assert_eq!(step(&rows, Some(0), -1), Some(0));
        // A selection that no longer exists starts over.
        assert_eq!(step(&rows, Some(40), 1), Some(0));
        assert_eq!(step(&[], None, 1), None);
    }

    #[test]
    fn the_attention_badge_lands_on_the_first_session_that_wants_the_user() {
        let store = store(vec![(
            "p1",
            vec![
                item("a"),
                Item {
                    status: SessionStatus::Running,
                    ..item("b")
                },
                Item {
                    status: SessionStatus::Error,
                    ..item("c")
                },
            ],
        )]);
        let rows = View::all(&store).rows(&store, None);
        // The failed one sorts to the top of its project.
        assert_eq!(first_waiting(&rows), Some(1));
        assert!(matches!(&rows[1], SidebarRow::Session { id, .. } if id.as_str() == "c"));
        let calm = store_of_idle();
        assert_eq!(first_waiting(&View::all(&calm).rows(&calm, None)), None);
    }

    fn store_of_idle() -> SessionStore<Item> {
        store(vec![("p1", vec![item("a")])])
    }

    #[test]
    fn ages_read_briefly() {
        const MIN: u64 = 60_000;
        assert_eq!(age_label(Locale::En, NOW, NOW), "now");
        assert_eq!(age_label(Locale::En, NOW, NOW - 59_000), "now");
        assert_eq!(age_label(Locale::En, NOW, NOW - 5 * MIN), "5m");
        assert_eq!(age_label(Locale::En, NOW, NOW - 59 * MIN), "59m");
        assert_eq!(age_label(Locale::En, NOW, NOW - HOUR), "1h");
        assert_eq!(age_label(Locale::En, NOW, NOW - 23 * HOUR), "23h");
        assert_eq!(age_label(Locale::En, NOW, NOW - 24 * HOUR), "1d");
        assert_eq!(age_label(Locale::En, NOW, NOW - 13 * 24 * HOUR), "13d");
        assert_eq!(age_label(Locale::En, NOW, NOW - 14 * 24 * HOUR), "2w");
        assert_eq!(age_label(Locale::En, NOW, NOW + HOUR), "now", "a clock that ran back");
        assert_eq!(age_label(Locale::ZhCn, NOW, NOW), "刚刚");
        assert_eq!(age_label(Locale::ZhCn, NOW, NOW - 3 * HOUR), "3 小时");
    }
}
