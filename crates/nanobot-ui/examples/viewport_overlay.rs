//! Acceptance check 4 for the shell rebuild (docs/ui-redesign-design.md §12):
//! what happens beside the native viewport surface.
//!
//! A real window: a session column on the left whose width sweeps back and
//! forth (a divider being dragged), the viewport taking the rest, and two
//! boxes that reach over the column's edge into the viewport — one painted in
//! place, one `deferred`. The viewport clears to solid magenta, so a
//! screenshot shows at a glance what ends up above it.
//!
//! It runs by itself for a few seconds, writes what it measured to the file
//! named by its first argument and exits. Take screenshots while it runs.
//!
//!     cargo run -p nanobot-ui --example viewport_overlay -- <report-file> <log-dir>

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    deferred, div, prelude::*, px, rgb, App, Bounds, Context, Size, Window, WindowBounds,
    WindowOptions,
};
use gpui_gpu_view::{viewport_element, Gpu, Viewport, ViewportContent, ViewportInput};

/// How long each phase lasts: still, sweeping, still.
const STILL: Duration = Duration::from_millis(2500);
const SWEEP: Duration = Duration::from_millis(3000);

/// The viewport's content: one solid colour, and a record of what it drew.
#[derive(Default)]
struct Magenta {
    frames: u64,
    sizes: Vec<(u32, u32)>,
    pointer_events: u64,
}

impl ViewportContent for Magenta {
    fn input(&mut self, _input: ViewportInput) -> bool {
        self.pointer_events += 1;
        false
    }

    fn draw(&mut self, gpu: &Gpu, view: &wgpu::TextureView, size: (u32, u32)) {
        self.frames += 1;
        if self.sizes.last() != Some(&size) {
            self.sizes.push(size);
        }
        let mut encoder = gpu
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 1.0,
                        g: 0.0,
                        b: 1.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
                depth_slice: None,
            })],
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
    }
}

struct Check {
    viewport: Rc<RefCell<Viewport<Magenta>>>,
    started: Instant,
    last_frame: Instant,
    /// Gaps between gpui frames while the divider sweeps.
    sweep_gaps: Vec<Duration>,
    report: std::path::PathBuf,
    done: bool,
}

impl Check {
    /// The session column's width now, and whether the divider is moving.
    fn column_width(&self) -> (f32, bool) {
        let elapsed = self.started.elapsed();
        if elapsed < STILL || elapsed >= STILL + SWEEP {
            return (440., false);
        }
        let phase = (elapsed - STILL).as_secs_f32() / SWEEP.as_secs_f32();
        // Three sweeps between 300 and 580 px.
        let swing = (phase * std::f32::consts::TAU * 3.).sin();
        (440. + 140. * swing, true)
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.done = true;
        let viewport = self.viewport.borrow();
        let content = &viewport.content;
        let mut gaps = self.sweep_gaps.clone();
        gaps.sort();
        let pick = |q: f32| {
            gaps.get(((gaps.len() as f32 * q) as usize).min(gaps.len().saturating_sub(1)))
                .copied()
                .unwrap_or_default()
        };
        let report = format!(
            "gpui frames during the sweep: {}\n\
             frame gap median {:?}, p95 {:?}, worst {:?}\n\
             viewport frames drawn: {}\n\
             distinct viewport sizes: {}\n\
             first sizes: {:?}\n\
             pointer events routed to the viewport: {}\n",
            gaps.len(),
            pick(0.5),
            pick(0.95),
            gaps.last().copied().unwrap_or_default(),
            content.frames,
            content.sizes.len(),
            &content.sizes[..content.sizes.len().min(6)],
            content.pointer_events,
        );
        let _ = std::fs::write(&self.report, report);
        cx.quit();
    }
}

impl Render for Check {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let (width, sweeping) = self.column_width();
        if sweeping {
            self.sweep_gaps.push(now - self.last_frame);
            // A resize changes what the viewport must draw.
            self.viewport.borrow_mut().invalidate();
        }
        self.last_frame = now;
        if !self.done && self.started.elapsed() > STILL * 2 + SWEEP {
            self.finish(cx);
        }
        window.request_animation_frame();

        // Both boxes start inside the column and reach 160 px past its edge.
        let reach = |top: f32, color: u32, label: &'static str| {
            div()
                .absolute()
                .left(px(width - 140.))
                .top(px(top))
                .w(px(300.))
                .h(px(90.))
                .bg(rgb(color))
                .border_2()
                .border_color(rgb(0x000000))
                .text_color(rgb(0x000000))
                .p_2()
                .child(label)
        };

        div()
            .size_full()
            .flex()
            .flex_row()
            .bg(rgb(0x1e2028))
            .text_color(rgb(0xffffff))
            .child(
                div()
                    .w(px(width))
                    .h_full()
                    .flex_none()
                    .border_r_1()
                    .border_color(rgb(0x555a66))
                    .p_3()
                    .child("session column")
                    .child(format!("width {width:.0}px")),
            )
            .child(div().flex_1().h_full().child(viewport_element(&self.viewport)))
            .child(reach(120., 0x35d07f, "PLAIN (painted in place)"))
            .child(deferred(reach(260., 0xffd23f, "DEFERRED")).with_priority(1))
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let report = std::path::PathBuf::from(args.next().expect("a report file"));
    if let Some(log_dir) = args.next() {
        nanobot_boot::diagnostics::init(std::path::Path::new(&log_dir), "viewport_overlay check");
    }

    gpui_platform::application().run(move |cx: &mut App| {
        gpui_component::init(cx);
        let bounds = Bounds::centered(
            None,
            Size {
                width: px(1100.),
                height: px(520.),
            },
            cx,
        );
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        cx.open_window(options, |_, cx| {
            cx.new(|_| Check {
                viewport: Rc::new(RefCell::new(Viewport::new(Magenta::default()))),
                started: Instant::now(),
                last_frame: Instant::now(),
                sweep_gaps: Vec::new(),
                report,
                done: false,
            })
        })
        .expect("a window");
        cx.activate(true);
    });
}
