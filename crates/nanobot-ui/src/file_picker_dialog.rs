//! The in-app file picker overlay: open and save dialogs for storage without
//! an OS dialog (the browser's origin-private file system). Ported from the
//! workbench framework's picker design; the state machine lives in
//! `product-kit` ([`product_kit::FilePicker`]), this module draws it over the
//! shell and drives the listing I/O.

use gpui::{
    deferred, div, prelude::*, px, AnyElement, Context, Div, ElementId, Entity, MouseButton,
    MouseDownEvent, Window,
};
use gpui_component::input::{Input, InputState};
use gpui_component::scroll::Scrollable as _;
use gpui_component::{ActiveTheme as _, StyledExt as _};
use product_kit::{Entry, FilePicker, Location};

use crate::icons::{icon, Icon};
use crate::model::SessionView;
use crate::shell::{palette, FilePick, HarnessShell, Palette};
use crate::widgets;

/// Everything the overlay draws, snapshotted from the shell.
struct Snap {
    title: &'static str,
    sources: Vec<(String, String)>,
    provider: String,
    trail: Vec<String>,
    listing: Option<Result<Vec<Entry>, String>>,
    typed: Option<String>,
    overwrite: bool,
    border: gpui::Hsla,
    muted: gpui::Hsla,
    fg: gpui::Hsla,
}

/// What a finished picker's async leg concludes with.
enum Outcome {
    /// A save pick: the bytes are in storage.
    Saved(Location),
    /// An open pick: the file's bytes.
    Loaded(Location, Vec<u8>),
}

impl<S: SessionView> HarnessShell<S> {
    /// Opens the file picker. `seed` is the suggested save name (a save
    /// picker when `Some`, an open picker when `None`); files filter by
    /// `extensions` (lowercase, no dot; empty shows every file). False when
    /// no in-app storage source is registered (the desktop: the product's
    /// own dialogs apply).
    pub fn open_file_picker(
        &mut self,
        save: Option<&str>,
        extensions: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let sources = self.storage.in_app_sources();
        let Some(picker) = FilePicker::new(save.map(str::to_string), extensions, sources) else {
            return false;
        };
        self.session_menu = None;
        self.settings_open = false;
        let seed = save.unwrap_or_default().to_string();
        self.file_picker = Some(picker);
        self.list_picker_folder(cx);
        if save.is_some() {
            // The seed rides the placeholder (the input starts empty); a
            // blank typed name falls back to it at confirm time.
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(crate::i18n::t(self.locale, "shell.pick_name_placeholder"))
            });
            self.file_picker_name_input = Some(input);
        } else {
            self.file_picker_name_input = None;
        }
        let _ = seed;
        true
    }

    /// Whether the picker is open.
    pub fn file_picker_is_open(&self) -> bool {
        self.file_picker.is_some()
    }

    /// Closes the picker without a choice.
    pub fn close_file_picker(&mut self) {
        self.file_picker = None;
        self.file_picker_name_input = None;
    }

    /// Registers the app's interest in a finished picker. On an open pick the
    /// file's bytes have already been read (`content`); on a save pick the
    /// write has already completed (the shell serialises the two I/O steps,
    /// so the callback observes finished work only).
    pub fn set_on_file_picked<F>(&mut self, callback: F)
    where
        F: Fn(FilePick) + 'static,
    {
        self.on_file_picked = Some(std::rc::Rc::new(callback));
    }

    /// Lists the folder the picker currently shows into its state.
    fn list_picker_folder(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.file_picker.as_ref() else {
            return;
        };
        let dir = picker.dir();
        let list = self.storage.list(&dir);
        let task = cx.spawn(async move |shell, cx| {
            let listing = list.await;
            let _ = shell.update(cx, |shell, cx| {
                if let Some(picker) = shell.file_picker.as_mut() {
                    picker.listed(&dir, listing);
                }
                cx.notify();
            });
        });
        task.detach();
    }

    /// A navigation step on the picker, then a fresh listing.
    fn navigate_picker(
        &mut self,
        step: impl FnOnce(&mut FilePicker),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(picker) = self.file_picker.as_mut() else {
            return;
        };
        step(picker);
        self.list_picker_folder(cx);
        cx.notify();
        let _ = window;
    }

    /// Finishes the picker with `picked`: an open pick reads the bytes here,
    /// a save pick writes the app's bytes, and the app callback observes the
    /// finished work.
    fn finish_file_picker(
        &mut self,
        picked: Option<Location>,
        cx: &mut Context<Self>,
    ) {
        let Some(location) = picked else {
            self.close_file_picker();
            cx.notify();
            return;
        };
        let save = self.file_picker.as_ref().map_or(false, |p| p.save.is_some());
        let write_bytes = if save {
            self.file_save_bytes.as_ref().map(|provide| provide())
        } else {
            None
        };
        self.close_file_picker();
        let storage = self.storage.clone();
        let task = cx.spawn(async move |shell, cx| {
            let outcome: Result<Outcome, String> = if save {
                match write_bytes {
                    Some(bytes) => storage
                        .write(&location, bytes)
                        .await
                        .map(|_| Outcome::Saved(location)),
                    None => Err("nothing to save".to_string()),
                }
            } else {
                storage
                    .read(&location)
                    .await
                    .map(|content| Outcome::Loaded(location, content))
            };
            let _ = shell.update(cx, |shell, cx| {
                if let Some(callback) = shell.on_file_picked.clone() {
                    match outcome {
                        Ok(Outcome::Saved(location)) => callback(FilePick {
                            save: true,
                            location,
                            content: None,
                        }),
                        Ok(Outcome::Loaded(location, content)) => callback(FilePick {
                            save: false,
                            location,
                            content: Some(content),
                        }),
                        Err(error) => log::warn!("file picker: {error}"),
                    }
                }
                cx.notify();
            });
        });
        task.detach();
    }

    /// The dialog, when open: deferred, so it also covers the viewport.
    pub(crate) fn file_picker_overlay(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let picker = self.file_picker.as_ref()?;
        let typed = self
            .file_picker_name_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string());
        let p = palette(cx);
        let typed_for_overwrite = typed.clone();
        let snap = Snap {
            title: if picker.save.is_some() {
                self.tr("shell.pick_save")
            } else {
                self.tr("shell.pick_open")
            },
            sources: picker.sources.clone(),
            provider: picker.provider.clone(),
            trail: picker
                .trail
                .iter()
                .enumerate()
                .map(|(depth, (_, name))| {
                    if depth == 0 {
                        self.tr("shell.pick_source_browser").to_string()
                    } else {
                        name.clone()
                    }
                })
                .collect(),
            listing: picker.listing.clone(),
            typed,
            overwrite: typed_for_overwrite
                .as_deref()
                .is_some_and(|name| picker.existing(&picker.file_name(name)).is_some()),
            border: p.border,
            muted: p.muted,
            fg: p.fg,
        };
        let backdrop = div()
            .id("harness-file-picker-backdrop")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui::black().opacity(0.35))
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p_4()
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _window, cx| {
                this.close_file_picker();
                cx.notify();
            }))
            .child(self.file_picker_card(snap, window, cx));
        Some(deferred(backdrop).with_priority(10).into_any_element())
    }
}

impl<S: SessionView> HarnessShell<S> {
    fn file_picker_card(&self, snap: Snap, window: &mut Window, cx: &mut Context<Self>) -> gpui::Stateful<Div> {
        let p = palette(cx);
        let mut listing = div()
            .id("harness-file-picker-listing")
            .flex()
            .flex_col()
            .gap(px(2.));
        match &snap.listing {
            None => {
                listing = listing.child(
                    div().px_2().py_1().text_sm().text_color(snap.muted).child(
                        crate::i18n::t(self.locale, "shell.pick_loading").to_string(),
                    ),
                );
            }
            Some(Err(error)) => {
                listing = listing.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(snap.muted)
                        .child(format!("{error}")),
                );
            }
            Some(Ok(entries)) if entries.is_empty() => {
                let _ = entries;
                listing = listing.child(
                    div().px_2().py_1().text_sm().text_color(snap.muted).child(
                        crate::i18n::t(self.locale, "shell.pick_empty").to_string(),
                    ),
                );
            }
            Some(Ok(entries)) => {
                for (index, entry) in entries.iter().enumerate() {
                    let id = ElementId::Name(format!("file-picker-entry-{index}").into());
                    let entry = entry.clone();
                    let open_mode = self.file_picker.as_ref().map_or(false, |p| p.save.is_none());
                    let provider = self
                        .file_picker
                        .as_ref()
                        .map_or(String::new(), |p| p.provider.clone());
                    listing = listing.child(
                        div()
                            .id(id)
                            .px_2()
                            .py_1()
                            .rounded(px(4.))
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .hover(|row| row.bg(snap.muted.opacity(0.15)))
                            .child(icon(
                                if entry.is_dir { Icon::Sidebar } else { Icon::Load },
                                snap.fg,
                            ))
                            .child(div().flex_1().text_sm().child(entry.name.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if entry.is_dir {
                                    let entry = entry.clone();
                                    this.navigate_picker(
                                        |picker| picker.enter(&entry),
                                        window,
                                        cx,
                                    );
                                } else if open_mode {
                                    // An open picker: a file row picks it.
                                    this.finish_file_picker(Some(Location::new(
                                        provider.clone(),
                                        entry.key.clone(),
                                    )), cx);
                                }
                            })),
                    );
                }
            }
        }

        // Breadcrumbs: each folder up the trail is a way back.
        let mut crumbs = div().flex().flex_row().gap_1().items_center();
        for (depth, name) in snap.trail.iter().enumerate() {
            if depth > 0 {
                crumbs = crumbs.child(div().text_sm().text_color(snap.muted).child("/"));
            }
            crumbs = crumbs.child(
                div()
                    .id(ElementId::Name(format!("file-picker-crumb-{depth}").into()))
                    .text_sm()
                    .px_1()
                    .rounded(px(3.))
                    .cursor_pointer()
                    .hover(|row| row.bg(snap.muted.opacity(0.15)))
                    .child(name.clone())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate_picker(|picker| picker.up_to(depth), window, cx);
                    })),
            );
        }

        let name_input = self.file_picker_name_input.clone();
        let confirm_label = if snap.overwrite {
            self.tr("shell.pick_confirm_overwrite")
        } else {
            self.tr("shell.pick_save")
        };

        let mut card = div()
            .id("harness-file-picker")
            .w(px(560.))
            .max_w_full()
            .rounded(px(8.))
            .border_1()
            .border_color(snap.border)
            .bg(cx.theme().background)
            .shadow_lg()
            .overflow_hidden()
            .flex()
            .flex_col()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .px_4()
                    .py_2()
                    .border_b_1()
                    .border_color(snap.border)
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(self.tr(snap.title)),
                    )
                    .child(
                        div()
                            .id("harness-file-picker-close")
                            .p_1()
                            .rounded(px(4.))
                            .cursor_pointer()
                            .hover(|d| d.bg(snap.muted.opacity(0.2)))
                            .child(icon(Icon::Close, snap.muted))
                            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, _window, cx| {
                                this.close_file_picker();
                                cx.notify();
                            })),
                    ),
            );

        // Sources on the left, listing on the right (workbench's layout).
        let mut sources = div().flex().flex_col().w(px(132.)).flex_none().gap(px(2.));
        for (id, label) in snap.sources.clone() {
            let selected = id == snap.provider;
            sources = sources.child(
                div()
                    .id(ElementId::Name(format!("file-picker-source-{id}").into()))
                    .px_2()
                    .py_1()
                    .rounded(px(4.))
                    .text_sm()
                    .cursor_pointer()
                    .when(selected, |row| row.bg(snap.muted.opacity(0.15)))
                    .hover(|row| row.bg(snap.muted.opacity(0.25)))
                    .child(label.clone())
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, window, cx| {
                        this.navigate_picker(|picker| picker.browse(&id), window, cx);
                    })),
            );
        }

        let mut footer = div()
            .px_4()
            .py_2()
            .border_t_1()
            .border_color(snap.border)
            .flex()
            .flex_row()
            .items_center()
            .gap_2();
        if let Some(input) = &name_input {
            footer = footer.child(div().flex_1().child(Input::new(&input.clone()).h(px(28.))));
        }
        footer = footer.child(widgets::button(
            "harness-file-picker-confirm",
            confirm_label,
            p.accent,
            cx,
            cx.listener(move |this, _event, window, cx| {
                let typed = this
                    .file_picker_name_input
                    .as_ref()
                    .map(|input| input.read(cx).value().to_string());
                // A blank typed name falls back to the picker's seed.
                let seed = this.file_picker.as_ref().and_then(|p| p.save.clone());
                let name = typed
                    .as_deref()
                    .map(str::trim)
                    .filter(|n| !n.is_empty())
                    .map(str::to_string)
                    .or(seed);
                let target = this
                    .file_picker
                    .as_ref()
                    .and_then(|picker| name.as_deref().and_then(|n| picker.target(n)));
                this.finish_file_picker(target, cx);
                let _ = window;
            }),
        ));

        card = card
            .child(div().px_4().py_2().child(crumbs))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .px_4()
                    .pb_2()
                    .child(sources)
                    .child(listing.flex_1().max_h(px(320.)).overflow_y_scroll()),
            );
        // A save picker confirms from the name input; an open picker has no
        // name to type, so the footer carries the hint instead.
        if name_input.is_some() {
            card = card.child(footer);
        }
        card
    }
}
