//! The menu of a session in the sidebar: rename, fork, archive, delete.
//!
//! Opened with the right mouse button on a session's row. It lists only what
//! the product gave a way to do; with nothing to list it does not open.
//! Deleting cannot be undone, so it asks twice: the first choice arms it and
//! the menu stays, the second does it.

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, MouseButton, MouseDownEvent, Pixels,
    Point, Window,
};
use gpui_component::ActiveTheme as _;

use crate::icons::{icon, Icon};
use crate::model::{SessionId, SessionView};
use crate::shell::{palette, HarnessShell};

/// Something the menu can do to a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionAction {
    Rename,
    Fork,
    Archive,
    Delete,
}

impl SessionAction {
    fn label_key(self) -> &'static str {
        match self {
            Self::Rename => "session.rename",
            Self::Fork => "session.fork",
            Self::Archive => "session.archive",
            Self::Delete => "session.delete",
        }
    }

    fn icon(self) -> Icon {
        match self {
            Self::Rename => Icon::ToolEdit,
            Self::Fork => Icon::Branch,
            Self::Archive => Icon::Archive,
            Self::Delete => Icon::ToolDelete,
        }
    }
}

/// What choosing an action came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Chosen {
    /// The product was asked; the menu is closed.
    Done,
    /// The title is to be edited in the row: that needs a window.
    Rename { session: SessionId, title: String },
    /// Deleting is armed; choosing it again deletes.
    ConfirmDelete,
    /// No menu, or nothing the product can do.
    Nothing,
}

/// The open menu.
pub(crate) struct SessionMenu {
    pub(crate) session: SessionId,
    pub(crate) title: String,
    /// Where the pointer was, in window coordinates.
    pub(crate) at: Point<Pixels>,
    pub(crate) delete_armed: bool,
}

const MENU_W: f32 = 200.;
const ROW_H: f32 = 28.;

/// Where the menu's corner goes so that all of it stays in the window.
#[must_use]
pub fn menu_origin(at: (f32, f32), rows: usize, window: (f32, f32)) -> (f32, f32) {
    let height = rows as f32 * ROW_H + 8.;
    (
        at.0.min(window.0 - MENU_W - 4.).max(4.),
        at.1.min(window.1 - height - 4.).max(4.),
    )
}

impl<S: SessionView> HarnessShell<S> {
    /// What forking a session does.
    pub fn set_on_fork<F: Fn(&SessionId) + 'static>(&mut self, callback: F) {
        self.on_fork = Some(std::rc::Rc::new(callback));
    }

    /// What deleting a session does. The shell has already asked twice.
    pub fn set_on_delete<F: Fn(&SessionId) + 'static>(&mut self, callback: F) {
        self.on_delete = Some(std::rc::Rc::new(callback));
    }

    /// What the menu of a session lists: the things the product can do.
    pub fn session_actions(&self) -> Vec<SessionAction> {
        [
            (SessionAction::Rename, self.on_rename.is_some()),
            (SessionAction::Fork, self.on_fork.is_some()),
            (SessionAction::Archive, self.on_archive.is_some()),
            (SessionAction::Delete, self.on_delete.is_some()),
        ]
        .into_iter()
        .filter_map(|(action, can)| can.then_some(action))
        .collect()
    }

    /// Asks the product to fork `session`.
    pub fn fork_session(&mut self, session: &SessionId) -> bool {
        match self.on_fork.clone() {
            Some(callback) => {
                callback(session);
                true
            }
            None => false,
        }
    }

    /// Asks the product to delete `session`, for good.
    pub fn delete_session(&mut self, session: &SessionId) -> bool {
        match self.on_delete.clone() {
            Some(callback) => {
                callback(session);
                self.sidebar_selected = None;
                true
            }
            None => false,
        }
    }

    /// Opens the menu of `session` at `at` (window coordinates). False when
    /// there is nothing it could list.
    pub fn open_session_menu(&mut self, session: SessionId, title: &str, at: (f32, f32)) -> bool {
        if self.session_actions().is_empty() {
            return false;
        }
        self.session_menu = Some(SessionMenu {
            session,
            title: title.to_string(),
            at: Point::new(px(at.0), px(at.1)),
            delete_armed: false,
        });
        true
    }

    /// Whether a session's menu is open.
    pub fn session_menu_is_open(&self) -> bool {
        self.session_menu.is_some()
    }

    /// Closes the menu. False when none was open.
    pub fn close_session_menu(&mut self) -> bool {
        self.session_menu.take().is_some()
    }

    /// Chooses `action` in the open menu.
    pub fn choose_session_action(&mut self, action: SessionAction) -> Chosen {
        let Some(menu) = self.session_menu.as_mut() else {
            return Chosen::Nothing;
        };
        if action == SessionAction::Delete && !menu.delete_armed {
            if self.on_delete.is_none() {
                return Chosen::Nothing;
            }
            menu.delete_armed = true;
            return Chosen::ConfirmDelete;
        }
        let Some(menu) = self.session_menu.take() else {
            return Chosen::Nothing;
        };
        let asked = match action {
            SessionAction::Rename => {
                return if self.on_rename.is_some() {
                    Chosen::Rename { session: menu.session, title: menu.title }
                } else {
                    Chosen::Nothing
                };
            }
            SessionAction::Fork => self.fork_session(&menu.session),
            SessionAction::Archive => self.archive_session(&menu.session),
            SessionAction::Delete => self.delete_session(&menu.session),
        };
        if asked {
            Chosen::Done
        } else {
            Chosen::Nothing
        }
    }

    /// The open menu, over everything: a click anywhere else closes it.
    pub(crate) fn session_menu_overlay(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let menu = self.session_menu.as_ref()?;
        let p = palette(cx);
        let actions = self.session_actions();
        let size = window.viewport_size();
        let (left, top) = menu_origin(
            (f32::from(menu.at.x), f32::from(menu.at.y)),
            actions.len(),
            (f32::from(size.width), f32::from(size.height)),
        );
        let armed = menu.delete_armed;
        let rows = actions.into_iter().enumerate().map(|(ix, action)| {
            let confirming = armed && action == SessionAction::Delete;
            let color = if action == SessionAction::Delete { p.danger } else { p.fg };
            let label = if confirming {
                self.tr("session.delete_confirm")
            } else {
                self.tr(action.label_key())
            };
            div()
                .id(("harness-session-action", ix))
                .h(px(ROW_H))
                .px_2()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_sm()
                .text_color(color)
                .cursor_pointer()
                .when(confirming, |d| d.bg(p.danger.opacity(0.14)))
                .hover(|d| d.bg(p.accent.opacity(0.25)))
                .child(icon(action.icon(), color))
                .child(div().flex_1().min_w_0().truncate().child(label))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this: &mut Self, _ev: &MouseDownEvent, window, cx| {
                        if let Chosen::Rename { session, title } =
                            this.choose_session_action(action)
                        {
                            this.begin_rename_in(session, &title, window, cx);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        });
        let card = div()
            .id("harness-session-menu")
            .absolute()
            .left(px(left))
            .top(px(top))
            .w(px(MENU_W))
            .py_1()
            .rounded(px(6.))
            .border_1()
            .border_color(p.border)
            .bg(cx.theme().background)
            .shadow_lg()
            .flex()
            .flex_col()
            .children(rows);
        let close = |this: &mut Self, _ev: &MouseDownEvent, _w: &mut Window, cx: &mut Context<Self>| {
            this.close_session_menu();
            cx.notify();
        };
        let backdrop = div()
            .id("harness-session-menu-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .on_mouse_down(MouseButton::Left, cx.listener(close))
            .on_mouse_down(MouseButton::Right, cx.listener(close))
            .child(card);
        Some(deferred(backdrop).with_priority(9).into_any_element())
    }
}
