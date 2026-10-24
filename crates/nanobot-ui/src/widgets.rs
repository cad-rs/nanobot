//! Small shell widgets: the pieces a product panel is built from, on top of
//! gpui-component.
//!
//! Nothing here reimplements a component. Where the library has one — `Button`,
//! `Label` — these call it, and what they add is a semantic colour slot: a panel
//! speaks `pass` / `fail` / `warn` / `waived`, the components speak theme variants,
//! and this is the mapping between them. The one shape the library does not have —
//! a flat, always-expanded list of rows — is composed from gpui primitives and says
//! so.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ClickEvent, ColorExt as _, ElementId, Hsla, SharedString, Window, div, hsla,
    prelude::*, px,
};
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_component::label::Label;

/// An item addressed by name and content hash.
///
/// The hash is part of the identity: a store keeps two variants of one item under
/// the same name, so a name alone cannot say which one is meant.
pub type HashedRef = (String, String);

/// A content hash in the short form the shell shows everywhere.
#[must_use]
pub fn short_hash(hash: &str) -> String {
    hash.chars().take(8).collect()
}

/// Truncate to `max` characters, with an ellipsis.
#[must_use]
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        text.chars().take(max).collect::<String>() + "…"
    }
}

/// A JSON value on one line, elided when it is too long to read.
#[must_use]
pub fn show(value: Option<&serde_json::Value>) -> String {
    value.map_or_else(|| "—".to_string(), |value| truncate(&value.to_string(), 24))
}

/// A one-line status in the colour the panel means.
#[must_use]
pub fn line(text: impl Into<SharedString>, color: Hsla) -> AnyElement {
    Label::new(text)
        .text_xs()
        .text_color(color)
        .into_any_element()
}

/// A small button in the colour the panel means.
///
/// Thin adapter over the library's `Button`: the custom variant carries the
/// semantic colour as a tinted background with matching text, and hover, disabled
/// and accessibility come with the component rather than being guessed at here.
/// `id` must be unique in the panel — lists repeat the same verb per row, so rows
/// suffix the row's identity.
#[must_use]
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    color: Hsla,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let variant = ButtonCustomVariant::new(cx)
        .color(color.opacity(0.12))
        .foreground(color)
        .hover(color.opacity(0.22));
    Button::new(id)
        .label(label)
        .custom(variant)
        .xsmall()
        .on_click(on_click)
        .into_any_element()
}

/// What a picker does with the item that was chosen.
#[allow(clippy::type_complexity)]
type PickHandler = Rc<dyn Fn(&HashedRef, &mut App) + 'static>;

/// A picker over items addressed by name and hash.
///
/// Composed from gpui primitives rather than built on a library component: the
/// useful surface is a flat, always-expanded list of a few rows, and the library's
/// nearest component (`Select`) is a *searchable dropdown* with its own delegate and
/// state entity — more machinery than the rows it would replace. If a list ever
/// needs search or keyboard navigation, this is where `Select` comes in.
///
/// `on_pick` decides what a choice means — the caller owns the panel — and whether
/// to re-render. It is shared across the rows, so each row gets the same answer for
/// the item it names.
#[must_use]
pub fn picker(
    label: &'static str,
    chosen: Option<&HashedRef>,
    items: &[(String, String)],
    on_pick: PickHandler,
) -> AnyElement {
    let mut column = div().flex().flex_col().gap_0p5().child(
        div()
            .text_xs()
            .text_color(hsla(0.6, 0.1, 0.5, 1.0))
            .child(label),
    );
    for item in items.iter().take(6).cloned() {
        let selected = chosen == Some(&item);
        let color = if selected {
            hsla(0.6, 0.8, 0.6, 1.0)
        } else {
            hsla(0.6, 0.1, 0.55, 1.0)
        };
        let caption = format!("{} {}", item.0, short_hash(&item.1));
        let on_pick = Rc::clone(&on_pick);
        column = column.child(
            div()
                .id(SharedString::from(format!("pick-{}", item.1)))
                .px_1()
                .py_0p5()
                .rounded(px(3.0))
                .when(selected, |d| d.bg(color.opacity(0.2)))
                .cursor_pointer()
                .text_xs()
                .text_color(color)
                .child(caption)
                .on_mouse_down(gpui::MouseButton::Left, move |_ev, _window, cx| {
                    on_pick(&item, cx);
                }),
        );
    }
    column.into_any_element()
}
