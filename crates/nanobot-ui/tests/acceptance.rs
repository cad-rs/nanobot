//! Acceptance checks for the shell rebuild (docs/ui-redesign-design.md §12):
//! does the pinned gpui-fast + gpui-component stack do what the design
//! assumes? Headless, on gpui's test platform.
//!
//! 1. the variable-height `list` follows its tail and updates in place;
//! 2. the markdown text view takes streamed appends cheaply and selects text;
//! 3. the text area holds atomic inline tokens (the `@` chips).
//!
//! The fourth check — overlays and divider drags beside the native viewport
//! surface — needs a real window; see examples/viewport_overlay.rs.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    div, list, point, px, AppContext as _, Context, Entity, FollowMode, IntoElement, ListAlignment,
    ListState, ParentElement as _, Render, ScrollDelta, ScrollEvent, Styled as _, TestAppContext,
    VisualTestContext, Window,
};
use gpui_component::input::{InlineToken, InputContent, Position, Textarea, TextareaState};
use gpui_component::text::{TextView, TextViewState};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        _ = window.draw(cx);
    });
}

// ---------------------------------------------------------------------------
// 1. The conversation list.

/// A 400×300 list whose items take their heights from a shared vector, so a
/// test can grow one (a streaming message) or add more (new entries).
struct Stream {
    state: ListState,
    heights: Rc<RefCell<Vec<f32>>>,
}

impl Render for Stream {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let heights = self.heights.clone();
        div().w(px(400.)).h(px(300.)).child(
            list(self.state.clone(), move |ix, _, _| {
                div()
                    .w_full()
                    .h(px(heights.borrow()[ix]))
                    .child(format!("entry {ix}"))
                    .into_any_element()
            })
            .size_full(),
        )
    }
}

fn stream(
    cx: &mut TestAppContext,
) -> (ListState, Rc<RefCell<Vec<f32>>>, &mut VisualTestContext) {
    let state = ListState::new(0, ListAlignment::Top, px(1000.));
    state.set_follow_mode(FollowMode::Tail);
    let heights = Rc::new(RefCell::new(Vec::new()));
    let (view_state, view_heights) = (state.clone(), heights.clone());
    let (_, cx) = cx.add_window_view(|_, _| Stream {
        state: view_state,
        heights: view_heights,
    });
    (state, heights, cx)
}

/// Appends `count` entries of `height` and tells the list.
fn append(state: &ListState, heights: &Rc<RefCell<Vec<f32>>>, count: usize, height: f32) {
    let old = heights.borrow().len();
    heights
        .borrow_mut()
        .extend(std::iter::repeat_n(height, count));
    state.splice(old..old, count);
}

/// The y of the last entry's bottom edge relative to the viewport's bottom:
/// zero when the list shows its very end.
fn gap_below_last(state: &ListState) -> f32 {
    let last = state.item_count() - 1;
    let item = state.bounds_for_item(last).expect("the last entry is laid out");
    f32::from(state.viewport_bounds().bottom() - item.bottom())
}

#[gpui::test]
fn the_list_follows_new_entries_and_a_growing_last_entry(cx: &mut TestAppContext) {
    let (state, heights, cx) = stream(cx);

    append(&state, &heights, 40, 30.);
    draw(cx);
    assert!(state.is_following_tail());
    assert_eq!(state.is_scrolled_to_end(), Some(true));
    assert_eq!(gap_below_last(&state), 0., "the end is in view");

    // New entries arrive.
    append(&state, &heights, 5, 50.);
    draw(cx);
    assert_eq!(gap_below_last(&state), 0., "still at the end");

    // The last entry grows, as a message does while it streams: only that
    // entry is remeasured.
    let last = state.item_count() - 1;
    for _ in 0..10 {
        heights.borrow_mut()[last] += 40.;
        state.remeasure_items(last..last + 1);
        draw(cx);
        assert_eq!(gap_below_last(&state), 0., "the growing entry stays in view");
    }
    assert!(state.is_following_tail());
}

#[gpui::test]
fn scrolling_up_pauses_following_and_returning_resumes_it(cx: &mut TestAppContext) {
    let (state, heights, cx) = stream(cx);
    append(&state, &heights, 40, 30.);
    draw(cx);

    // The user scrolls up with the wheel.
    cx.simulate_event(ScrollEvent {
        position: point(px(200.), px(150.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(200.))),
        ..Default::default()
    });
    draw(cx);
    assert!(!state.is_following_tail(), "scrolling up stops the following");
    let held = state.logical_scroll_top();

    // Entries keep arriving and the last one grows: the view stays put.
    append(&state, &heights, 5, 50.);
    let last = state.item_count() - 1;
    heights.borrow_mut()[last] += 200.;
    state.remeasure_items(last..last + 1);
    draw(cx);
    let now = state.logical_scroll_top();
    assert_eq!(
        (now.item_ix, now.offset_in_item),
        (held.item_ix, held.offset_in_item),
        "what the user is reading does not move"
    );
    assert_eq!(state.is_scrolled_to_end(), Some(false));

    // "Jump to bottom": following resumes.
    state.set_follow_mode(FollowMode::Tail);
    draw(cx);
    assert!(state.is_following_tail());
    assert_eq!(gap_below_last(&state), 0.);
    append(&state, &heights, 1, 80.);
    draw(cx);
    assert_eq!(gap_below_last(&state), 0.);
}

#[gpui::test]
fn entries_can_be_removed_and_replaced_in_place(cx: &mut TestAppContext) {
    let (state, heights, cx) = stream(cx);
    append(&state, &heights, 20, 30.);
    draw(cx);

    // A rewind drops the tail of the conversation.
    heights.borrow_mut().truncate(12);
    state.splice(12..20, 0);
    draw(cx);
    assert_eq!(state.item_count(), 12);
    assert_eq!(gap_below_last(&state), 0.);

    // One entry is replaced by three (a merged tool summary expanding).
    heights.borrow_mut().splice(5..6, [10., 10., 10.]);
    state.splice(5..6, 3);
    draw(cx);
    assert_eq!(state.item_count(), 14);
}

// ---------------------------------------------------------------------------
// 2. Streaming markdown.

struct Markdown {
    state: Entity<TextViewState>,
}

impl Render for Markdown {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(px(440.))
            .child(TextView::new(&self.state).selectable(true))
    }
}

/// An assistant answer of roughly `paragraphs` blocks: prose, lists and code.
fn answer(paragraphs: usize) -> String {
    (0..paragraphs)
        .map(|ix| match ix % 4 {
            0 => format!("## Step {ix}\n\nThe bracket needs **four** M6 holes on a `40 mm` pitch.\n\n"),
            1 => "- pick the top face\n- sketch the pattern\n- cut through all\n\n".to_string(),
            2 => "```rhai\nlet holes = pattern(4, 40.0);\nbody.cut(holes);\n```\n\n".to_string(),
            _ => "Verification passed: the wall thickness stays above the minimum everywhere.\n\n"
                .to_string(),
        })
        .collect()
}

#[gpui::test]
fn markdown_takes_streamed_appends_and_selects(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    // NB_MARKDOWN_PARAGRAPHS scales the answer, to see how the cost grows.
    let paragraphs = std::env::var("NB_MARKDOWN_PARAGRAPHS")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(120);
    let source = answer(paragraphs);
    let (view, cx) = cx.add_window_view(|_, cx| Markdown {
        state: cx.new(|cx| TextViewState::markdown("", cx)),
    });
    let state = view.read_with(cx, |view, _| view.state.clone());

    // Stream it in 24-byte chunks, drawing a frame after each, as the shell
    // would on its 16 ms tick.
    let mut frames = Vec::new();
    let mut rest = source.as_str();
    while !rest.is_empty() {
        let mut cut = rest.len().min(24);
        while !rest.is_char_boundary(cut) {
            cut += 1;
        }
        let (chunk, tail) = rest.split_at(cut);
        rest = tail;
        let started = Instant::now();
        state.update(cx, |state, cx| state.push_str(chunk, cx));
        cx.run_until_parked();
        cx.update(|window, cx| {
            _ = window.draw(cx);
        });
        frames.push(started.elapsed());
    }

    let total: Duration = frames.iter().sum();
    let mean = total / frames.len() as u32;
    let worst = frames.iter().max().copied().unwrap();
    let tenth = frames.len() / 10;
    let early = frames[..tenth].iter().sum::<Duration>() / tenth as u32;
    let late = frames[frames.len() - tenth..].iter().sum::<Duration>() / tenth as u32;
    eprintln!(
        "markdown stream: {} bytes in {} appends; per append+frame mean {mean:?}, worst {worst:?}, \
         first tenth {early:?}, last tenth {late:?}",
        source.len(),
        frames.len(),
    );

    // The whole answer arrived, and it can be selected and copied out.
    state.update(cx, |state, cx| state.select_all(cx));
    let selected = state.read_with(cx, |state, _| state.selected_text());
    assert!(selected.contains("four"), "{selected:.80}");
    assert!(selected.contains("let holes = pattern(4, 40.0);"));
    let last_heading = format!("Step {}", (paragraphs - 1) / 4 * 4);
    assert!(selected.contains(&last_heading), "the end of the answer is there");

    // One text view per answer is fine at this size and not beyond it: the
    // cost of an append grows with how much text is laid out on screen
    // (measured: ~1.8 ms at 8 KB, ~16 ms at 32 KB, ~50 ms at 80 KB). Splitting
    // the text into several views does not help by itself; drawing only the
    // views in sight does (`markdown_in_segments_stays_cheap`). The budget
    // here holds the default 8 KB inside a 16 ms frame.
    if paragraphs == 120 {
        assert!(mean < Duration::from_millis(6), "mean append took {mean:?}");
        assert!(late < Duration::from_millis(10), "late appends took {late:?}");
    }
}

/// A long answer as a run of text views, a new one begun at a paragraph
/// break once the current one passes `SEGMENT` bytes. Only the last `VISIBLE`
/// are drawn, as a virtualised list draws only the entries in view.
struct Segmented {
    segments: Vec<Entity<TextViewState>>,
}

const SEGMENT: usize = 2000;
const VISIBLE: usize = 3;

impl Render for Segmented {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(440.)).children(
            self.segments
                .iter()
                .skip(self.segments.len().saturating_sub(VISIBLE))
                .map(|state| TextView::new(state).selectable(true)),
        )
    }
}

#[gpui::test]
fn markdown_in_segments_stays_cheap(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let source = answer(1200); // ~80 KB
    let (view, cx) = cx.add_window_view(|_, cx| Segmented {
        segments: vec![cx.new(|cx| TextViewState::markdown("", cx))],
    });

    let mut frames = Vec::new();
    let mut in_segment = 0;
    for line in source.split_inclusive('\n') {
        let started = Instant::now();
        view.update(cx, |view, cx| {
            // A blank line ends a block: the safe place to start a new view.
            if line == "\n" && in_segment > SEGMENT {
                view.segments
                    .push(cx.new(|cx| TextViewState::markdown("", cx)));
                in_segment = 0;
                cx.notify();
            } else {
                in_segment += line.len();
                let last = view.segments.last().expect("one segment").clone();
                last.update(cx, |state, cx| state.push_str(line, cx));
            }
        });
        draw(cx);
        frames.push(started.elapsed());
    }

    let tenth = frames.len() / 10;
    let early = frames[..tenth].iter().sum::<Duration>() / tenth as u32;
    let late = frames[frames.len() - tenth..].iter().sum::<Duration>() / tenth as u32;
    let worst = frames.iter().max().copied().unwrap();
    let segments = view.read_with(cx, |view, _| view.segments.len());
    eprintln!(
        "segmented markdown: {} bytes in {} appends over {segments} views; first tenth {early:?}, \
         last tenth {late:?}, worst {worst:?}",
        source.len(),
        frames.len(),
    );
    assert!(late < Duration::from_millis(8), "late appends took {late:?}");

// The answer ends where it should, in the view that is on screen.    let last = view.read_with(cx, |view, _| view.segments.last().unwrap().clone());    last.update(cx, |state, cx| state.select_all(cx));    let tail = last.read_with(cx, |state, _| state.selected_text());    assert!(tail.contains("Step 1196"), "{tail:.80}");    assert!(segments > 30, "the answer was split into {segments} views");
}

// ---------------------------------------------------------------------------
// 3. Inline tokens in the composer.

struct Composer {
    state: Entity<TextareaState>,
}

impl Render for Composer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(440.)).child(Textarea::new(&self.state))
    }
}

fn composer(cx: &mut TestAppContext) -> (Entity<TextareaState>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let (view, cx) = cx.add_window_view(|window, cx| Composer {
        state: cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 8)),
    });
    let state = view.read_with(cx, |view, _| view.state.clone());
    state.update_in(cx, |state, window, cx| state.focus(window, cx));
    draw(cx);
    (state, cx)
}

#[gpui::test]
fn a_mention_becomes_one_atomic_token(cx: &mut TestAppContext) {
    let (state, cx) = composer(cx);

    // The user typed "@Bod" and picked "Body1" from the completion list: the
    // typed range is replaced by a token.
    cx.simulate_input("cut holes in @Bod");
    state.update_in(cx, |state, window, cx| {
        let end = state.value().len();
        state
            .replace_range_with_token(
                end - 4..end,
                InlineToken::new("feature:body1", "@Body1").with_label("Body1"),
                window,
                cx,
            )
            .expect("a text area takes tokens");
    });
    draw(cx);
    state.read_with(cx, |state, _| {
        assert_eq!(state.value().as_ref(), "cut holes in @Body1");
        let tokens = state.tokens();
        assert_eq!(tokens.len(), 1);
        assert_eq!(tokens[0].token().id().as_ref(), "feature:body1");
        assert_eq!(tokens[0].range(), 13..19);
        assert_eq!(state.cursor(), 19, "the caret sits after the token");
        assert!(
            state.range_to_bounds(&(13..19)).is_some(),
            "the token has a place on screen"
        );
    });

    // Typing continues after it, and the token's range holds.
    cx.simulate_input(" please");
    state.read_with(cx, |state, _| {
        assert_eq!(state.value().as_ref(), "cut holes in @Body1 please");
        assert_eq!(state.tokens()[0].range(), 13..19);
    });

    // What is sent keeps the reference, not just its text.
    let content: InputContent = state.read_with(cx, |state, _| state.content());
    assert_eq!(content.tokens()[0].token().id().as_ref(), "feature:body1");
}

#[gpui::test]
fn backspace_removes_a_token_whole(cx: &mut TestAppContext) {
    let (state, cx) = composer(cx);
    state.update_in(cx, |state, window, cx| {
        let content = InputContent::new("use @Body1")
            .with_token(4..10, InlineToken::new("feature:body1", "@Body1"))
            .unwrap();
        state.set_value(content, window, cx);
        let end = state.value().len();
        state.set_cursor_position(Position::new(0, end as u32), window, cx);
    });
    draw(cx);

    cx.simulate_keystrokes("backspace");
    state.read_with(cx, |state, _| {
        assert_eq!(state.value().as_ref(), "use ", "not \"use @Body\"");
        assert!(state.tokens().is_empty());
    });
}
