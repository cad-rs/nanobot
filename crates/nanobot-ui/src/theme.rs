//! Theme switching for the shell: dark / light, one entry point.
//!
//! The whole shell derives its colours per frame from the gpui-component
//! global theme ([`crate::shell`]'s palette reads `cx.theme()` in every
//! element builder), so switching is a single call: `Theme::change` swaps the
//! global theme and refreshes the window. There is no second palette table to
//! keep in sync — unlike workbench-rs, whose shell colours live in a static
//! copy that a switch has to flip separately.
//!
//! The choice persists through the harness's user settings under
//! [`THEME_KEY`] (`ui.theme`); [`apply_from_settings`] is the launch-time
//! entry (after `gpui_component::init`, before the first window), and
//! [`set_theme`] is the runtime entry (writes the setting, then applies).

use std::sync::{Arc, Mutex};

use gpui::{px, App, Window};
use nanobot_boot::Settings;
use serde_json::Value;

/// The settings key the theme name lives under.
pub const THEME_KEY: &str = "ui.theme";

/// The settings key the UI locale lives under.
pub const LOCALE_KEY: &str = "ui.locale";

/// The base font size (px). gpui-component's default is 16.
pub const FONT_SIZE_KEY: &str = "ui.font.size";
/// The UI font family; empty/unset means the platform default.
pub const FONT_FAMILY_KEY: &str = "ui.font.family";
/// The monospace family, for code and measurements.
pub const FONT_MONO_FAMILY_KEY: &str = "ui.font.mono_family";

/// The theme kinds the shell ships. Anything unrecognised reads as dark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    Dark,
    Light,
}

impl ThemeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    /// Parses a stored theme name. Unknown values fall back to dark — a
    /// hand-edited settings file must not brick the shell.
    pub fn from_name(name: &str) -> Self {
        if name.eq_ignore_ascii_case("light") {
            Self::Light
        } else {
            Self::Dark
        }
    }

    /// The theme this kind toggles to.
    pub fn toggled(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }
}

/// Applies the theme to every themed surface: the gpui-component global
/// theme, which the shell's palette and every gpui-component widget derive
/// from. With a window in hand the window is refreshed in the same call;
/// without one (launch time, before the first window opens) the next frame
/// picks the theme up.
pub fn apply(kind: ThemeKind, window: Option<&mut Window>, cx: &mut App) {
    let mode = match kind {
        ThemeKind::Dark => gpui_component::ThemeMode::Dark,
        ThemeKind::Light => gpui_component::ThemeMode::Light,
    };
    gpui_component::Theme::change(mode, window, cx);
}

/// The theme the settings name, dark when unset.
#[must_use]
pub fn kind_from_settings(settings: &Settings) -> ThemeKind {
    ThemeKind::from_name(&settings.get_str(THEME_KEY, ThemeKind::Dark.as_str()))
}

/// Launch-time entry: applies what the settings name (dark when unset).
/// Call after `gpui_component::init` and before the first window opens.
pub fn apply_from_settings(settings: &Settings, cx: &mut App) -> ThemeKind {
    let kind = kind_from_settings(settings);
    apply(kind, None, cx);
    kind
}

/// Applies the font settings to the gpui-component theme.
///
/// The base size and the two families are public fields on `Theme`, and
/// every component reads them when it builds (through the typography
/// tokens), so a window refresh is all the propagation needed — there is
/// no second font table to keep in sync.
pub fn apply_font(settings: &Settings, window: Option<&mut Window>, cx: &mut App) {
    let size = settings.get(FONT_SIZE_KEY).and_then(Value::as_f64);
    let family = settings.get(FONT_FAMILY_KEY).and_then(Value::as_str);
    let mono = settings.get(FONT_MONO_FAMILY_KEY).and_then(Value::as_str);
    if size.is_none() && family.is_none() && mono.is_none() {
        return;
    }
    {
        let theme = cx.global_mut::<gpui_component::Theme>();
        if let Some(size) = size {
            theme.font_size = px(size as f32);
        }
        if let Some(family) = family {
            theme.font_family = family.into();
        }
        if let Some(mono) = mono {
            theme.mono_font_family = mono.into();
        }
    }
    gpui_component::Theme::sync_base(cx);
    if let Some(window) = window {
        window.refresh();
    }
}

/// Writes the theme choice through the settings store, then applies it.
/// The write goes through [`Settings::set`], so it reaches the file
/// immediately; an unwritable store degrades to an in-memory switch, never
/// an error surface.
pub fn set_theme(
    settings: &Arc<Mutex<Settings>>,
    kind: ThemeKind,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    if let Ok(mut store) = settings.lock() {
        let _ = store.set(THEME_KEY, serde_json::json!(kind.as_str()));
    }
    apply(kind, window, cx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_keys_are_distinct_from_the_theme_and_locale_keys() {
        for key in [FONT_SIZE_KEY, FONT_FAMILY_KEY, FONT_MONO_FAMILY_KEY] {
            assert_ne!(key, THEME_KEY);
            assert_ne!(key, LOCALE_KEY);
        }
    }

    #[test]
    fn names_round_trip_and_unknown_reads_as_dark() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            assert_eq!(ThemeKind::from_name(kind.as_str()), kind);
        }
        assert_eq!(ThemeKind::from_name("LIGHT"), ThemeKind::Light);
        assert_eq!(ThemeKind::from_name("nonsense"), ThemeKind::Dark);
        assert_eq!(ThemeKind::from_name(""), ThemeKind::Dark);
    }

    #[test]
    fn toggling_flips_both_kinds() {
        assert_eq!(ThemeKind::Dark.toggled(), ThemeKind::Light);
        assert_eq!(ThemeKind::Light.toggled(), ThemeKind::Dark);
    }

    #[test]
    fn settings_without_a_theme_read_as_dark() {
        let settings = Settings::in_memory();
        assert_eq!(kind_from_settings(&settings), ThemeKind::Dark);

        let mut named = Settings::in_memory();
        named.set(THEME_KEY, serde_json::json!("light")).unwrap();
        assert_eq!(kind_from_settings(&named), ThemeKind::Light);
    }
}
