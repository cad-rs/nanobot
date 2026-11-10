//! The shell's icons: one SVG file per icon under `assets/icons/`, drawn in
//! the colour of the text around it.
//!
//! Every icon the shell shows goes through [`Icon`], so replacing the artwork
//! is replacing files: keep the file names, a 16×16 view box and
//! `currentColor`, and nothing else has to change. The files shipped now are
//! placeholders.
//!
//! The files are compiled into the crate and served by [`Assets`]. gpui loads
//! SVGs through the application's asset source, so the product registers it at
//! startup — alone, or in front of its own:
//!
//! ```ignore
//! gpui_platform::application().with_assets(nanobot_ui::Assets::over(product_assets))
//! ```
//!
//! Without it the shell still works; its icons are simply not drawn.

use std::borrow::Cow;

use gpui::{px, svg, AssetSource, Hsla, SharedString, Styled as _, Svg};

/// Where the icons live in the application's asset namespace.
const PREFIX: &str = "nanobot-ui/icons/";

macro_rules! icons {
    ($($variant:ident => $file:literal,)*) => {
        /// An icon of the shell.
        #[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
        pub enum Icon {
            $($variant,)*
        }

        impl Icon {
            /// Every icon, for a gallery or a check that each file exists.
            pub const ALL: &'static [Icon] = &[$(Icon::$variant,)*];

            /// The file's name under `assets/icons/`, without `.svg`.
            pub fn name(self) -> &'static str {
                match self {
                    $(Icon::$variant => $file,)*
                }
            }

            fn bytes(self) -> &'static [u8] {
                match self {
                    $(Icon::$variant => include_bytes!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/assets/icons/",
                        $file,
                        ".svg"
                    )),)*
                }
            }
        }
    };
}

icons! {
    // chrome
    Sidebar => "sidebar",
    Dock => "dock",
    Viewport => "viewport",
    Chat => "chat",
    Account => "account",
    Settings => "settings",
    Close => "close",
    Plus => "plus",
    More => "more",
    // global operations
    Undo => "undo",
    Redo => "redo",
    Save => "save",
    Load => "load",
    Import => "import",
    // folding and movement
    ChevronRight => "chevron-right",
    ChevronDown => "chevron-down",
    ArrowDown => "arrow-down",
    // the conversation
    Thinking => "thinking",
    Plan => "plan",
    CheckPass => "check-pass",
    CheckFail => "check-fail",
    Error => "error",
    TurnEnd => "turn-end",
    Anchor => "anchor",
    Branch => "branch",
    // tool kinds
    ToolRead => "tool-read",
    ToolEdit => "tool-edit",
    ToolDelete => "tool-delete",
    ToolMove => "tool-move",
    ToolSearch => "tool-search",
    ToolExecute => "tool-execute",
    ToolThink => "tool-think",
    ToolFetch => "tool-fetch",
    ToolOther => "tool-other",
    // the composer
    Send => "send",
    Queue => "queue",
    Stop => "stop",
    Mention => "mention",
    Attach => "attach",
}

impl Icon {
    /// The asset path gpui loads this icon from.
    pub fn path(self) -> SharedString {
        format!("{PREFIX}{}.svg", self.name()).into()
    }

    /// The fold mark of a row that is open or closed.
    pub fn chevron(open: bool) -> Self {
        if open {
            Self::ChevronDown
        } else {
            Self::ChevronRight
        }
    }

    /// The icon of a tool kind.
    pub fn tool(kind: crate::transcript::ToolKind) -> Self {
        use crate::transcript::ToolKind;
        match kind {
            ToolKind::Read => Self::ToolRead,
            ToolKind::Edit => Self::ToolEdit,
            ToolKind::Delete => Self::ToolDelete,
            ToolKind::Move => Self::ToolMove,
            ToolKind::Search => Self::ToolSearch,
            ToolKind::Execute => Self::ToolExecute,
            ToolKind::Think => Self::ToolThink,
            ToolKind::Fetch => Self::ToolFetch,
            ToolKind::Other => Self::ToolOther,
        }
    }
}

/// `icon` at 14 px in `color`: the size that sits on a line of small text.
pub fn icon(icon: Icon, color: Hsla) -> Svg {
    sized(icon, 14., color)
}

/// `icon` at `size` px in `color`.
pub fn sized(icon: Icon, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon.path())
        .size(px(size))
        .flex_none()
        .text_color(color)
}

/// The shell's icons as a gpui asset source, in front of `F` (the product's
/// own assets, or `()` for none).
#[derive(Clone, Copy, Debug, Default)]
pub struct Assets<F = ()>(pub F);

impl Assets {
    /// The shell's icons alone.
    pub fn new() -> Self {
        Self(())
    }
}

impl<F> Assets<F> {
    /// The shell's icons first, then everything `fallback` has.
    pub fn over(fallback: F) -> Self {
        Self(fallback)
    }
}

impl<F: AssetSource> AssetSource for Assets<F> {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let ours = path
            .strip_prefix(PREFIX)
            .and_then(|file| file.strip_suffix(".svg"))
            .and_then(|name| Icon::ALL.iter().find(|icon| icon.name() == name));
        match ours {
            Some(icon) => Ok(Some(Cow::Borrowed(icon.bytes()))),
            None => self.0.load(path),
        }
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        let mut found = self.0.list(path)?;
        if PREFIX.starts_with(path) || path.starts_with(PREFIX.trim_end_matches('/')) {
            found.extend(Icon::ALL.iter().map(|icon| icon.path()));
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_a_themable_svg() {
        let assets = Assets::new();
        for icon in Icon::ALL {
            let bytes = assets
                .load(&icon.path())
                .unwrap()
                .unwrap_or_else(|| panic!("{icon:?} has no file"));
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(text.trim_start().starts_with("<svg"), "{icon:?}");
            assert!(text.contains("viewBox=\"0 0 16 16\""), "{icon:?} keeps the 16×16 box");
            assert!(
                text.contains("currentColor"),
                "{icon:?} must take the colour it is drawn in"
            );
        }
    }

    #[test]
    fn names_are_unique() {
        let mut names: Vec<&str> = Icon::ALL.iter().map(|icon| icon.name()).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total);
    }

    struct Product;

    impl AssetSource for Product {
        fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
            Ok((path == "logo.svg").then_some(Cow::Borrowed(b"<svg/>".as_slice())))
        }
        fn list(&self, _path: &str) -> gpui::Result<Vec<SharedString>> {
            Ok(vec!["logo.svg".into()])
        }
    }

    #[test]
    fn the_products_assets_stay_reachable_behind_the_shells() {
        let assets = Assets::over(Product);
        assert!(assets.load("logo.svg").unwrap().is_some());
        assert!(assets.load(&Icon::Send.path()).unwrap().is_some());
        assert!(assets.load("nanobot-ui/icons/nope.svg").unwrap().is_none());
        assert!(assets.load("missing.svg").unwrap().is_none());
        let listed = assets.list("nanobot-ui/icons").unwrap();
        assert!(listed.contains(&Icon::Send.path()) && listed.contains(&"logo.svg".into()));
    }
}
