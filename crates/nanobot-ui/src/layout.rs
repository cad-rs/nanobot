//! How the shell divides the window (docs/ui-redesign-design.md §3).
//!
//! Four regions side by side: the session sidebar, the session column, the
//! viewport and the right dock. The viewport is the main area and takes what
//! is left; the other three have widths the user can drag, and the sidebar and
//! the dock can be hidden. As the window narrows the layout folds in three
//! steps, each at a named width:
//!
//! | below | what gives way |
//! |---|---|
//! | [`HIDE_SIDEBAR_BELOW`] | the sidebar hides |
//! | [`HIDE_DOCK_BELOW`] | the right dock hides |
//! | [`STACK_BELOW`] | column and viewport stop sharing the width: one at a time |
//!
//! Folding never rewrites what the user chose: [`Layout`] is the preference
//! that is saved, and [`LayoutState`] holds what the window's width does to it
//! at the moment. A toggle on a narrow window is a temporary override, dropped
//! when the window crosses a breakpoint again.
//!
//! Everything here is arithmetic on numbers, so it is tested without a window.
//! The pixel values are starting points: the survey behind the design found no
//! source that states them for a desktop window.

use nanobot_boot::Settings;
use serde_json::Value;

pub const SIDEBAR_DEFAULT: f32 = 280.;
pub const SIDEBAR_MIN: f32 = 240.;
pub const SIDEBAR_MAX: f32 = 480.;
/// The sidebar never takes more than this share of the window.
pub const SIDEBAR_MAX_SHARE: f32 = 0.4;

pub const COLUMN_DEFAULT: f32 = 440.;
pub const COLUMN_MIN: f32 = 360.;
pub const COLUMN_MAX: f32 = 760.;

pub const DOCK_DEFAULT: f32 = 300.;
pub const DOCK_MIN: f32 = 240.;
pub const DOCK_MAX: f32 = 560.;

/// The viewport keeps at least this much while the others are dragged.
pub const VIEWPORT_MIN: f32 = 320.;

pub const HIDE_SIDEBAR_BELOW: f32 = 1280.;
pub const HIDE_DOCK_BELOW: f32 = 1040.;
pub const STACK_BELOW: f32 = 760.;

/// The settings keys the layout persists under.
pub const SIDEBAR_OPEN_KEY: &str = "ui.layout.sidebar_open";
pub const SIDEBAR_WIDTH_KEY: &str = "ui.layout.sidebar_width";
pub const COLUMN_WIDTH_KEY: &str = "ui.layout.column_width";
pub const DOCK_OPEN_KEY: &str = "ui.layout.dock_open";
pub const DOCK_WIDTH_KEY: &str = "ui.layout.dock_width";

/// What the user chose: saved, and restored at the next launch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub sidebar_open: bool,
    pub sidebar_width: f32,
    pub column_width: f32,
    pub dock_open: bool,
    pub dock_width: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self {
            sidebar_open: true,
            sidebar_width: SIDEBAR_DEFAULT,
            column_width: COLUMN_DEFAULT,
            dock_open: true,
            dock_width: DOCK_DEFAULT,
        }
    }
}

impl Layout {
    /// The saved layout; anything missing or out of range falls back to the
    /// default, so a hand-edited or older settings file cannot break the shell.
    pub fn load(settings: &Settings) -> Self {
        let default = Self::default();
        let flag = |key, fallback| {
            settings
                .get(key)
                .and_then(Value::as_bool)
                .unwrap_or(fallback)
        };
        let width = |key, fallback: f32, min: f32, max: f32| {
            settings
                .get(key)
                .and_then(Value::as_f64)
                .map(|w| w as f32)
                .filter(|w| w.is_finite())
                .map_or(fallback, |w| w.clamp(min, max))
        };
        Self {
            sidebar_open: flag(SIDEBAR_OPEN_KEY, default.sidebar_open),
            sidebar_width: width(SIDEBAR_WIDTH_KEY, default.sidebar_width, SIDEBAR_MIN, SIDEBAR_MAX),
            column_width: width(COLUMN_WIDTH_KEY, default.column_width, COLUMN_MIN, COLUMN_MAX),
            dock_open: flag(DOCK_OPEN_KEY, default.dock_open),
            dock_width: width(DOCK_WIDTH_KEY, default.dock_width, DOCK_MIN, DOCK_MAX),
        }
    }

    /// Writes the layout through the settings store.
    pub fn store(&self, settings: &mut Settings) -> Result<(), String> {
        settings.set(SIDEBAR_OPEN_KEY, Value::Bool(self.sidebar_open))?;
        settings.set(SIDEBAR_WIDTH_KEY, Value::from(self.sidebar_width.round()))?;
        settings.set(COLUMN_WIDTH_KEY, Value::from(self.column_width.round()))?;
        settings.set(DOCK_OPEN_KEY, Value::Bool(self.dock_open))?;
        settings.set(DOCK_WIDTH_KEY, Value::from(self.dock_width.round()))
    }
}

/// How far the window's width has folded the layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fold {
    /// All four regions fit.
    Wide,
    /// The sidebar gives way.
    NoSidebar,
    /// The right dock gives way too.
    NoDock,
    /// Column and viewport no longer share the width.
    Stacked,
}

impl Fold {
    pub fn of(window_width: f32) -> Self {
        if window_width < STACK_BELOW {
            Self::Stacked
        } else if window_width < HIDE_DOCK_BELOW {
            Self::NoDock
        } else if window_width < HIDE_SIDEBAR_BELOW {
            Self::NoSidebar
        } else {
            Self::Wide
        }
    }
}

/// Which of the two shares the whole width when the layout is [`Fold::Stacked`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stacked {
    #[default]
    Column,
    Viewport,
}

/// A draggable boundary between two regions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Divider {
    /// Between the sidebar and the session column.
    Sidebar,
    /// Between the session column and the viewport.
    Column,
    /// Between the viewport and the right dock.
    Dock,
}

/// The widths to draw: `None` is a region that is not shown.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regions {
    pub fold: Fold,
    pub sidebar: Option<f32>,
    pub column: Option<f32>,
    /// The viewport's width, when it is shown. It is what is left.
    pub viewport: Option<f32>,
    pub dock: Option<f32>,
}

/// The layout as the window shows it now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LayoutState {
    layout: Layout,
    /// The fold the overrides below belong to.
    fold: Option<Fold>,
    /// A toggle made while the window was too narrow for the region: it wins
    /// until the window crosses a breakpoint.
    sidebar_override: Option<bool>,
    dock_override: Option<bool>,
    stacked: Stacked,
}

impl LayoutState {
    pub fn new(layout: Layout) -> Self {
        Self {
            layout,
            ..Self::default()
        }
    }

    /// The preference to save.
    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Call with the window's width before anything else each frame: crossing
    /// a breakpoint drops the temporary overrides.
    pub fn observe(&mut self, window_width: f32) {
        let fold = Fold::of(window_width);
        if self.fold != Some(fold) {
            self.fold = Some(fold);
            self.sidebar_override = None;
            self.dock_override = None;
        }
    }

    fn sidebar_shown(&self, fold: Fold) -> bool {
        match fold {
            Fold::Stacked => false,
            Fold::Wide => self.layout.sidebar_open,
            _ => self.sidebar_override.unwrap_or(false),
        }
    }

    fn dock_shown(&self, fold: Fold, has_dock: bool) -> bool {
        has_dock
            && match fold {
                Fold::Stacked => false,
                Fold::Wide | Fold::NoSidebar => self.layout.dock_open,
                Fold::NoDock => self.dock_override.unwrap_or(false),
            }
    }

    /// The regions for a window `window_width` wide. `has_dock` is whether any
    /// panel is registered for the right dock.
    pub fn regions(&self, window_width: f32, has_dock: bool) -> Regions {
        let fold = Fold::of(window_width);
        if fold == Fold::Stacked {
            let (column, viewport) = match self.stacked {
                Stacked::Column => (Some(window_width), None),
                Stacked::Viewport => (None, Some(window_width)),
            };
            return Regions {
                fold,
                sidebar: None,
                column,
                viewport,
                dock: None,
            };
        }

        let sidebar = self.sidebar_shown(fold).then(|| {
            self.layout
                .sidebar_width
                .min(window_width * SIDEBAR_MAX_SHARE)
                .max(SIDEBAR_MIN)
        });
        let dock = self.dock_shown(fold, has_dock).then_some(self.layout.dock_width);
        // The column yields before the viewport drops under its minimum, down
        // to its own minimum; past that the viewport is what shrinks.
        let taken = sidebar.unwrap_or(0.) + dock.unwrap_or(0.);
        let column = self
            .layout
            .column_width
            .min(window_width - taken - VIEWPORT_MIN)
            .max(COLUMN_MIN);
        let viewport = (window_width - taken - column).max(0.);
        Regions {
            fold,
            sidebar,
            column: Some(column),
            viewport: Some(viewport),
            dock,
        }
    }

    /// Shows or hides the sidebar. On a window wide enough this is the
    /// preference; on a narrower one it lasts until the next breakpoint.
    /// Returns whether the preference changed (and should be saved).
    pub fn toggle_sidebar(&mut self, window_width: f32) -> bool {
        self.observe(window_width);
        match Fold::of(window_width) {
            Fold::Wide => {
                self.layout.sidebar_open = !self.layout.sidebar_open;
                true
            }
            Fold::Stacked => false,
            fold => {
                self.sidebar_override = Some(!self.sidebar_shown(fold));
                false
            }
        }
    }

    /// Shows the sidebar if it is hidden (the attention badge's click).
    pub fn reveal_sidebar(&mut self, window_width: f32) -> bool {
        self.observe(window_width);
        if Fold::of(window_width) == Fold::Stacked || self.sidebar_shown(Fold::of(window_width)) {
            return false;
        }
        self.toggle_sidebar(window_width)
    }

    /// Shows or hides the right dock; as [`Self::toggle_sidebar`].
    pub fn toggle_dock(&mut self, window_width: f32) -> bool {
        self.observe(window_width);
        match Fold::of(window_width) {
            Fold::Wide | Fold::NoSidebar => {
                self.layout.dock_open = !self.layout.dock_open;
                true
            }
            Fold::NoDock => {
                self.dock_override = Some(!self.dock_override.unwrap_or(false));
                false
            }
            Fold::Stacked => false,
        }
    }

    /// Which of column and viewport a stacked layout shows.
    pub fn stacked(&self) -> Stacked {
        self.stacked
    }

    pub fn set_stacked(&mut self, stacked: Stacked) {
        self.stacked = stacked;
    }

    /// The width `divider`'s own region has now: where a drag starts from.
    pub fn width_at(&self, divider: Divider, window_width: f32, has_dock: bool) -> Option<f32> {
        let regions = self.regions(window_width, has_dock);
        match divider {
            Divider::Sidebar => regions.sidebar,
            Divider::Column => regions.column.filter(|_| regions.viewport.is_some()),
            Divider::Dock => regions.dock,
        }
    }

    /// Gives `divider`'s region the width `width` (where a drag has taken it),
    /// keeping every region inside its limits and the viewport above its
    /// minimum. Returns whether a width changed.
    pub fn resize(
        &mut self,
        divider: Divider,
        width: f32,
        window_width: f32,
        has_dock: bool,
    ) -> bool {
        if self.width_at(divider, window_width, has_dock).is_none() {
            return false;
        }
        let regions = self.regions(window_width, has_dock);
        let before = self.layout;
        let sidebar = regions.sidebar.unwrap_or(0.);
        let column = regions.column.unwrap_or(0.);
        let dock = regions.dock.unwrap_or(0.);
        match divider {
            Divider::Sidebar => {
                let room = window_width - dock - COLUMN_MIN - VIEWPORT_MIN;
                let max = SIDEBAR_MAX.min(window_width * SIDEBAR_MAX_SHARE).min(room);
                self.layout.sidebar_width = width.clamp(SIDEBAR_MIN, max.max(SIDEBAR_MIN));
            }
            Divider::Column => {
                let max = COLUMN_MAX.min(window_width - sidebar - dock - VIEWPORT_MIN);
                self.layout.column_width = width.clamp(COLUMN_MIN, max.max(COLUMN_MIN));
            }
            Divider::Dock => {
                let max = DOCK_MAX.min(window_width - sidebar - column - VIEWPORT_MIN);
                self.layout.dock_width = width.clamp(DOCK_MIN, max.max(DOCK_MIN));
            }
        }
        self.layout != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> LayoutState {
        LayoutState::new(Layout::default())
    }

    fn total(regions: &Regions) -> f32 {
        [regions.sidebar, regions.column, regions.viewport, regions.dock]
            .into_iter()
            .flatten()
            .sum()
    }

    #[test]
    fn a_wide_window_shows_all_four_and_the_viewport_takes_the_rest() {
        let regions = state().regions(1600., true);
        assert_eq!(regions.fold, Fold::Wide);
        assert_eq!(regions.sidebar, Some(280.));
        assert_eq!(regions.column, Some(440.));
        assert_eq!(regions.dock, Some(300.));
        assert_eq!(regions.viewport, Some(580.), "what is left");
        assert_eq!(total(&regions), 1600.);

        // No panel registered for the right dock: its width goes to the viewport.
        let regions = state().regions(1600., false);
        assert_eq!(regions.dock, None);
        assert_eq!(regions.viewport, Some(880.));
    }

    #[test]
    fn the_window_folds_in_three_steps() {
        let state = state();
        let at = |width| state.regions(width, true);

        let regions = at(1279.);
        assert_eq!(regions.fold, Fold::NoSidebar);
        assert_eq!(regions.sidebar, None);
        assert!(regions.dock.is_some());

        let regions = at(1039.);
        assert_eq!(regions.fold, Fold::NoDock);
        assert_eq!((regions.sidebar, regions.dock), (None, None));
        assert_eq!(regions.column, Some(440.));
        assert_eq!(regions.viewport, Some(599.));

        let regions = at(759.);
        assert_eq!(regions.fold, Fold::Stacked);
        assert_eq!(regions.column, Some(759.), "the column alone, by default");
        assert_eq!(regions.viewport, None);
    }

    #[test]
    fn a_stacked_window_shows_the_column_or_the_viewport() {
        let mut state = state();
        state.set_stacked(Stacked::Viewport);
        let regions = state.regions(700., true);
        assert_eq!((regions.column, regions.viewport), (None, Some(700.)));
        // Widening brings both back, whatever was showing.
        let regions = state.regions(1100., true);
        assert!(regions.column.is_some() && regions.viewport.is_some());
    }

    #[test]
    fn the_column_yields_before_the_viewport_drops_under_its_minimum() {
        let mut state = state();
        state.layout.column_width = 700.;
        // 800 wide, no sidebar or dock: 700 would leave the viewport 100.
        let regions = state.regions(800., false);
        assert_eq!(regions.column, Some(480.));
        assert_eq!(regions.viewport, Some(VIEWPORT_MIN));
        assert_eq!(state.layout().column_width, 700., "the preference is untouched");
    }

    #[test]
    fn hiding_the_sidebar_on_a_wide_window_is_the_preference() {
        let mut state = state();
        assert!(state.toggle_sidebar(1600.), "to be saved");
        assert!(!state.layout().sidebar_open);
        let regions = state.regions(1600., true);
        assert_eq!(regions.sidebar, None);
        assert_eq!(regions.viewport, Some(860.), "the viewport gets the width");
        assert!(state.toggle_sidebar(1600.));
        assert_eq!(state.regions(1600., true).sidebar, Some(280.));
    }

    #[test]
    fn a_narrow_window_hides_the_sidebar_without_changing_the_preference() {
        let mut state = state();
        state.observe(1200.);
        assert_eq!(state.regions(1200., true).sidebar, None);
        assert!(state.layout().sidebar_open, "still wanted");

        // Opened by hand on the narrow window: shown, but nothing to save.
        assert!(!state.toggle_sidebar(1200.));
        assert!(state.regions(1200., true).sidebar.is_some());
        assert!(state.layout().sidebar_open);
        // And closed by hand again.
        assert!(!state.toggle_sidebar(1200.));
        assert_eq!(state.regions(1200., true).sidebar, None);

        // Back on a wide window the preference applies as it was.
        state.toggle_sidebar(1200.);
        state.observe(1600.);
        assert!(state.regions(1600., true).sidebar.is_some());
        state.observe(1200.);
        assert_eq!(
            state.regions(1200., true).sidebar,
            None,
            "the override did not outlive the breakpoint"
        );
    }

    #[test]
    fn the_attention_badge_reveals_a_hidden_sidebar_and_leaves_a_shown_one() {
        let mut state = state();
        assert!(!state.reveal_sidebar(1600.), "already shown");
        state.toggle_sidebar(1600.);
        assert!(state.reveal_sidebar(1600.));
        assert!(state.regions(1600., true).sidebar.is_some());

        state.observe(1100.);
        assert!(!state.reveal_sidebar(1100.), "an override, not a preference");
        assert!(state.regions(1100., true).sidebar.is_some());
        assert!(!state.reveal_sidebar(1100.), "and it stays");
        assert!(state.regions(1100., true).sidebar.is_some());
    }

    #[test]
    fn the_dock_folds_like_the_sidebar() {
        let mut state = state();
        assert!(state.toggle_dock(1600.));
        assert_eq!(state.regions(1600., true).dock, None);
        assert!(state.toggle_dock(1600.));

        assert_eq!(state.regions(1000., true).dock, None);
        assert!(!state.toggle_dock(1000.));
        assert_eq!(state.regions(1000., true).dock, Some(300.));
        assert!(state.layout().dock_open);
    }

    #[test]
    fn the_sidebar_never_takes_more_than_its_share() {
        let mut state = state();
        state.layout.sidebar_width = 480.;
        state.toggle_sidebar(1100.); // shown by hand on a narrow window
        let regions = state.regions(1100., false);
        assert_eq!(regions.sidebar, Some(440.), "40% of 1100");
    }

    #[test]
    fn resizing_a_region_stays_inside_every_limit() {
        let mut state = state();
        let (w, dock) = (1600., true);

        // The sidebar takes the width asked for, between its limits.
        assert!(state.resize(Divider::Sidebar, 320., w, dock));
        assert_eq!(state.layout().sidebar_width, 320.);
        state.resize(Divider::Sidebar, 50., w, dock);
        assert_eq!(state.layout().sidebar_width, SIDEBAR_MIN);
        state.resize(Divider::Sidebar, 900., w, dock);
        assert_eq!(state.layout().sidebar_width, SIDEBAR_MAX);

        // So does the column.
        state.resize(Divider::Sidebar, 300., w, dock);
        state.resize(Divider::Column, 500., w, dock);
        assert_eq!(state.layout().column_width, 500.);
        state.resize(Divider::Column, 10., w, dock);
        assert_eq!(state.layout().column_width, COLUMN_MIN);
        // It cannot push the viewport under its minimum: 1600-300-300-320.
        state.resize(Divider::Column, 1500., w, dock);
        assert_eq!(state.layout().column_width, 680.);
        assert_eq!(state.regions(w, dock).viewport, Some(VIEWPORT_MIN));

        // And the dock.
        state.resize(Divider::Column, 440., w, dock);
        state.resize(Divider::Dock, 400., w, dock);
        assert_eq!(state.layout().dock_width, 400.);
        state.resize(Divider::Dock, 10., w, dock);
        assert_eq!(state.layout().dock_width, DOCK_MIN);
        state.resize(Divider::Dock, 1600., w, dock);
        assert_eq!(state.layout().dock_width, 540., "1600-300-440-320");

        let regions = state.regions(w, dock);
        assert_eq!(total(&regions), w);
    }

    #[test]
    fn a_divider_of_a_hidden_region_does_nothing() {
        let mut state = state();
        state.toggle_sidebar(1600.);
        assert!(!state.resize(Divider::Sidebar, 400., 1600., true));
        assert!(!state.resize(Divider::Dock, 400., 1600., false));
        assert!(!state.resize(Divider::Column, 400., 700., true), "stacked");
        assert_eq!(state.layout().sidebar_width, SIDEBAR_DEFAULT);
    }

    #[test]
    fn the_layout_survives_a_round_trip_through_settings() {
        let mut settings = Settings::in_memory();
        assert_eq!(Layout::load(&settings), Layout::default(), "nothing saved yet");

        let layout = Layout {
            sidebar_open: false,
            sidebar_width: 312.4,
            column_width: 520.,
            dock_open: false,
            dock_width: 410.,
        };
        layout.store(&mut settings).unwrap();
        assert_eq!(
            Layout::load(&settings),
            Layout {
                sidebar_width: 312.,
                ..layout
            },
            "whole pixels"
        );
    }

    #[test]
    fn a_damaged_settings_file_falls_back_region_by_region() {
        let mut settings = Settings::in_memory();
        settings.set(SIDEBAR_WIDTH_KEY, Value::from(9000)).unwrap();
        settings.set(COLUMN_WIDTH_KEY, Value::from("wide")).unwrap();
        settings.set(DOCK_WIDTH_KEY, Value::from(10)).unwrap();
        settings.set(SIDEBAR_OPEN_KEY, Value::from("yes")).unwrap();
        let layout = Layout::load(&settings);
        assert_eq!(layout.sidebar_width, SIDEBAR_MAX, "clamped");
        assert_eq!(layout.column_width, COLUMN_DEFAULT, "not a number");
        assert_eq!(layout.dock_width, DOCK_MIN, "clamped");
        assert!(layout.sidebar_open, "not a flag");
    }
}
