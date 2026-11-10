//! The shell's motion: four marks, each meaning one thing.
//!
//! | mark | means |
//! |---|---|
//! | [`dots`] | the agent is producing an answer |
//! | [`pulse`] | something waits for the user |
//! | [`shimmer`] | this text stands in for something still coming |
//! | [`spinner`] | a tool (or a session) is running |
//!
//! Nothing else in the shell moves. Each mark has a still form that says the
//! same thing, drawn when motion is reduced — by the shell's own setting
//! ([`REDUCE_MOTION_KEY`]) or by the application
//! ([`gpui::App::reduce_motion`]) — so no meaning is carried by movement
//! alone.
//!
//! A repeating animation redraws the window while it is on screen. The marks
//! cap their frame rate ([`FPS`]): they are small, and the shell's `render`
//! runs on every one of those frames.

use std::time::Duration;

use gpui::{
    div, percentage, prelude::*, px, Animation, AnimationExt as _, AnyElement, App, ElementId, Hsla,
    Transformation,
};

use crate::icons::{sized, Icon};
use crate::model::SessionView;
use crate::shell::HarnessShell;
use crate::transcript::{ToolStatus, TurnPhase};

/// The settings key of the shell's own "reduce motion" switch.
pub const REDUCE_MOTION_KEY: &str = "ui.reduce_motion";

/// Frames per second the marks redraw at.
const FPS: f32 = 24.;

/// What a state is shown with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// Nothing moves: the state is at rest.
    None,
    /// The agent is producing an answer.
    Dots,
    /// Something waits for the user.
    Pulse,
    /// Work is running.
    Spinner,
}

impl Mark {
    /// The mark of a turn in `phase`.
    #[must_use]
    pub fn of_turn(phase: TurnPhase) -> Self {
        match phase {
            TurnPhase::Idle | TurnPhase::Error => Self::None,
            TurnPhase::Generating => Self::Dots,
            TurnPhase::AwaitingApproval
            | TurnPhase::AwaitingClarification
            | TurnPhase::AwaitingDecision => Self::Pulse,
        }
    }

    /// The mark of a tool call in `status`.
    #[must_use]
    pub fn of_tool(status: ToolStatus) -> Self {
        match status {
            ToolStatus::Running => Self::Spinner,
            ToolStatus::AwaitingApproval => Self::Pulse,
            ToolStatus::Done
            | ToolStatus::Failed
            | ToolStatus::Cancelled
            | ToolStatus::Rejected => Self::None,
        }
    }
}

/// How opaque dot `index` of `count` is at `t` (0..1 through one cycle): a
/// wave that passes over the dots from first to last.
#[must_use]
pub fn dot_opacity(t: f32, index: usize, count: usize) -> f32 {
    const LOW: f32 = 0.25;
    let offset = index as f32 / (count.max(1) as f32 + 1.);
    let phase = (t - offset).rem_euclid(1.);
    // One smooth bump per cycle, in the first half of the dot's own phase.
    let bump = if phase < 0.5 {
        (phase * 2. * std::f32::consts::PI).sin()
    } else {
        0.
    };
    LOW + (1. - LOW) * bump
}

/// How opaque a pulsing or shimmering element is at `t` (0..1 through one
/// cycle): down to `low` and back, smoothly.
#[must_use]
pub fn breath(t: f32, low: f32) -> f32 {
    let wave = (1. + (t * 2. * std::f32::consts::PI).cos()) / 2.;
    low + (1. - low) * wave
}

fn cycle(millis: u64) -> Animation {
    Animation::new(Duration::from_millis(millis))
        .repeat()
        .with_max_fps(FPS)
}

/// A turning mark, `size` px: work is running. Still, it is the same mark at
/// rest.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla, still: bool) -> AnyElement {
    let mark = sized(Icon::Spinner, size, color);
    if still {
        return mark.into_any_element();
    }
    mark.with_animation(id, cycle(900), |mark, t| {
        mark.with_transformation(Transformation::rotate(percentage(t)))
    })
    .into_any_element()
}

/// Three dots a wave runs over: an answer is being produced. Still, they are
/// three even dots.
pub fn dots(id: impl Into<ElementId>, color: Hsla, still: bool) -> AnyElement {
    const COUNT: usize = 3;
    let row = move |opacity: &dyn Fn(usize) -> f32| {
        div()
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap(px(3.))
            .children((0..COUNT).map(|ix| {
                div()
                    .size(px(4.))
                    .rounded(px(2.))
                    .bg(color.opacity(opacity(ix)))
            }))
    };
    if still {
        return row(&|_| 0.6).into_any_element();
    }
    div()
        .flex_none()
        .with_animation(id, cycle(1200), move |holder, t| {
            holder.child(row(&|ix| dot_opacity(t, ix, COUNT)))
        })
        .into_any_element()
}

/// `element` breathing: it waits for the user. Still, it is drawn as it is —
/// what waits is already in the warning colour.
pub fn pulse<E>(id: impl Into<ElementId>, element: E, still: bool) -> AnyElement
where
    E: Styled + IntoElement + 'static,
{
    breathing(id, element, 1400, 0.45, still)
}

/// `element` fading slowly in and out: it stands in for something still
/// coming. Still, it is drawn as it is.
pub fn shimmer<E>(id: impl Into<ElementId>, element: E, still: bool) -> AnyElement
where
    E: Styled + IntoElement + 'static,
{
    breathing(id, element, 2000, 0.5, still)
}

fn breathing<E>(
    id: impl Into<ElementId>,
    element: E,
    millis: u64,
    low: f32,
    still: bool,
) -> AnyElement
where
    E: Styled + IntoElement + 'static,
{
    if still {
        return element.into_any_element();
    }
    element
        .with_animation(id, cycle(millis), move |element, t| {
            element.opacity(breath(t, low))
        })
        .into_any_element()
}

/// The mark `mark` at the size of a line of small text, in `color`.
/// `Mark::Pulse` is a breathing dot; `Mark::None` is nothing.
pub fn mark(mark: Mark, id: impl Into<ElementId>, color: Hsla, still: bool) -> Option<AnyElement> {
    match mark {
        Mark::None => None,
        Mark::Dots => Some(dots(id, color, still)),
        Mark::Spinner => Some(spinner(id, 12., color, still)),
        Mark::Pulse => Some(pulse(
            id,
            div().size(px(8.)).flex_none().rounded(px(4.)).bg(color),
            still,
        )),
    }
}



impl<S: SessionView> HarnessShell<S> {
    /// Whether the shell's own "reduce motion" switch is on.
    pub fn reduce_motion(&self) -> bool {
        self.reduce_motion
    }

    /// Turns the shell's "reduce motion" switch, and remembers it through the
    /// settings store when one is shared (`ui.reduce_motion`).
    pub fn set_reduce_motion(&mut self, reduce: bool) {
        self.reduce_motion = reduce;
        self.save_setting(REDUCE_MOTION_KEY, serde_json::json!(reduce));
    }

    /// Whether the marks are drawn still: the shell's switch, or the
    /// application's.
    pub(crate) fn still(&self, cx: &App) -> bool {
        self.reduce_motion || cx.reduce_motion()
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_state_has_one_mark() {
        assert_eq!(Mark::of_turn(TurnPhase::Idle), Mark::None);
        assert_eq!(Mark::of_turn(TurnPhase::Error), Mark::None);
        assert_eq!(Mark::of_turn(TurnPhase::Generating), Mark::Dots);
        for waiting in [
            TurnPhase::AwaitingApproval,
            TurnPhase::AwaitingClarification,
            TurnPhase::AwaitingDecision,
        ] {
            assert_eq!(Mark::of_turn(waiting), Mark::Pulse);
        }

        assert_eq!(Mark::of_tool(ToolStatus::Running), Mark::Spinner);
        assert_eq!(Mark::of_tool(ToolStatus::AwaitingApproval), Mark::Pulse);
        for over in [
            ToolStatus::Done,
            ToolStatus::Failed,
            ToolStatus::Cancelled,
            ToolStatus::Rejected,
        ] {
            assert_eq!(Mark::of_tool(over), Mark::None);
        }
    }

    #[test]
    fn the_wave_passes_over_the_dots_in_order() {
        // Each dot peaks a quarter of a cycle after its own start, one after
        // the other.
        let peak = |ix: usize| {
            (0..100)
                .map(|step| step as f32 / 100.)
                .max_by(|a, b| dot_opacity(*a, ix, 3).total_cmp(&dot_opacity(*b, ix, 3)))
                .unwrap()
        };
        assert!(peak(0) < peak(1) && peak(1) < peak(2));
        for ix in 0..3 {
            for step in 0..100 {
                let opacity = dot_opacity(step as f32 / 100., ix, 3);
                assert!((0.25..=1.0).contains(&opacity), "dot {ix} at {step}: {opacity}");
            }
        }
    }

    #[test]
    fn a_breath_starts_and_ends_full_and_never_vanishes() {
        assert!((breath(0., 0.45) - 1.).abs() < 1e-6);
        assert!((breath(1., 0.45) - 1.).abs() < 1e-4, "the loop has no jump");
        assert!((breath(0.5, 0.45) - 0.45).abs() < 1e-6);
        for step in 0..=100 {
            let opacity = breath(step as f32 / 100., 0.45);
            assert!((0.45..=1.0).contains(&opacity));
        }
    }
}
