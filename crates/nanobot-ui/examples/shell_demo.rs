//! The shell in a real window, on made-up sessions: what to look at while the
//! rebuild (docs/ui-redesign-design.md) goes on.
//!
//!     cargo run -p nanobot-ui --example shell_demo
//!
//! Layout — things to try:
//! - drag the three dividers; the one beside the viewport must keep following
//!   the pointer while it is over the viewport;
//! - Ctrl+Alt+J (or the toolbar's first button) hides and shows the sidebar,
//!   Ctrl+Alt+B (or the title bar's last button) the right dock;
//! - narrow the window: the sidebar goes below 1280 px, the dock below 1040,
//!   and below 760 the session column and the viewport take turns;
//! - quit and start again: widths and what was hidden come back.
//!
//! The conversation stream:
//! - the first session shows one of every kind of row; click the folded ones;
//! - "法兰初稿（流式）" streams a long answer over and over: the end stays in
//!   view until you scroll up, and "回到底部" takes you back.
//!
//! The layout is saved in `shell_demo/settings.json` under the user's config
//! directory; delete it to start over.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use gpui::{
    div, prelude::*, px, AnyElement, App, Bounds, Size, WindowBounds, WindowOptions,
};
use nanobot_gpu::{
    viewport_element, Gpu, Viewport, ViewportContent, ViewportInput, ViewportProjection,
    ViewportSlot,
};
use nanobot_ui::{
    BranchMarker, Conversation, Entry, HarnessShell, PanelArea, PanelDef, PlanPhase, PlanStep,
    Project, ProjectId, SessionId, SessionStatus, SessionStore, SessionView, StepStatus,
    ToolCall, ToolKind, ToolStatus, WorkspaceView,
};

// ---- a stand-in renderer: a colour that drifts as the pointer drags ----

#[derive(Default)]
struct Scene {
    frames: u64,
    hue: f32,
    last: Option<f32>,
}

impl ViewportContent for Scene {
    fn input(&mut self, input: ViewportInput) -> bool {
        let x = input.mouse_pos.map(|p| f32::from(p.x));
        let changed = match (input.mouse_down, self.last, x) {
            (true, Some(last), Some(x)) if x != last => {
                self.hue = (self.hue + (x - last) / 600.).rem_euclid(1.);
                true
            }
            _ => false,
        };
        self.last = if input.mouse_down { x } else { None };
        changed
    }

    fn draw(&mut self, gpu: &Gpu, view: &wgpu::TextureView, _size: (u32, u32)) {
        self.frames += 1;
        let (r, g, b) = (
            0.10 + 0.25 * (self.hue * std::f32::consts::TAU).sin().abs(),
            0.14_f32,
            0.22 + 0.25 * (self.hue * std::f32::consts::TAU).cos().abs(),
        );
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
                        r: f64::from(r),
                        g: f64::from(g),
                        b: f64::from(b),
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

struct DemoViewport;

impl ViewportSlot for DemoViewport {
    type Handle = Rc<RefCell<Viewport<Scene>>>;
    type Scene = ();

    fn new_handle() -> Self::Handle {
        Rc::new(RefCell::new(Viewport::new(Scene::default())))
    }
    fn update_scene(handle: &Self::Handle, _scene: &()) {
        handle.borrow_mut().invalidate();
    }
    fn projection(_handle: &Self::Handle) -> Option<ViewportProjection> {
        Some(ViewportProjection::Orthographic)
    }
    fn painted_frames(handle: &Self::Handle) -> u64 {
        handle.borrow().content.frames
    }
    fn element(handle: &Self::Handle) -> AnyElement {
        viewport_element(handle)
    }
}

// ---- made-up sessions ----

#[derive(Clone)]
struct Demo {
    id: SessionId,
    title: String,
    status: SessionStatus,
    /// Shared with the timer in `main` that streams into the running session.
    conversation: Rc<RefCell<Conversation>>,
    plan: Vec<PlanStep>,
    plan_phase: PlanPhase,
}

/// One of every kind of entry, so every kind of row is on screen.
fn showcase() -> Vec<Entry> {
    let mesher: String = (1..=24)
        .map(|n| format!("pass {n:>2}: {} elements\n", 1200 + n * 340))
        .collect();
    vec![
        Entry::user(1, "在支架顶面开 4 个 M6 通孔，孔距 40 mm。"),
        Entry::thinking(
            2,
            "需要先确认顶面的范围，再决定阵列的基准。\n顶面是 Body1.TopFace，法向 +Z；孔阵列以面的中心为基准。",
            Some(1800),
        ),
        Entry::assistant(
            3,
            "好的。我先读取 **Body1** 的顶面，再生成孔阵列。\n\n\
             1. 读取顶面的范围\n2. 以面中心为基准生成 `2 × 2` 阵列\n3. 验证壁厚\n\n\
             ```rhai\nlet holes = pattern(4, 40.0);\nbody.cut(holes);\n```\n",
        ),
        Entry::tool(
            4,
            ToolCall::new(ToolKind::Read, "read_feature(Body1.TopFace)", ToolStatus::Done)
                .with_output("area 4800 mm², normal +Z"),
        ),
        Entry::tool(
            5,
            ToolCall::new(ToolKind::Read, "read_feature(Body1)", ToolStatus::Done),
        ),
        Entry::tool(
            6,
            ToolCall::new(ToolKind::Search, "find_holes(Body1)", ToolStatus::Done),
        ),
        Entry::tool(
            7,
            ToolCall::new(
                ToolKind::Edit,
                "pattern_holes(count=4, pitch=40)",
                ToolStatus::Done,
            )
            .with_input("{ \"face\": \"Body1.TopFace\", \"diameter\": 6.6 }")
            .with_output("4 holes cut"),
        )
        .with_anchor("Body1.TopFace"),
        Entry::tool(
            8,
            ToolCall::new(ToolKind::Execute, "run_mesher(size=2)", ToolStatus::Done)
                .with_output(mesher),
        ),
        Entry {
            kind: nanobot_ui::EntryKind::Check {
                name: "壁厚不小于 3 mm".to_string(),
                passed: false,
                details: Some("孔 3 处壁厚 2.1 mm，小于最小值 3 mm".to_string()),
            },
            ..Entry::check(9, "", false)
        },
        Entry::tool(
            10,
            ToolCall::new(ToolKind::Delete, "remove_feature(Hole3)", ToolStatus::Rejected),
        ),
        Entry::tool(
            11,
            ToolCall::new(
                ToolKind::Edit,
                "move_pattern(offset=1.5)",
                ToolStatus::AwaitingApproval,
            )
            .with_input("{ \"axis\": \"X\", \"offset\": 1.5 }"),
        ),
        Entry::plan(
            12,
            vec![
                PlanStep {
                    text: "读取顶面".to_string(),
                    status: StepStatus::Done,
                },
                PlanStep {
                    text: "生成孔阵列".to_string(),
                    status: StepStatus::Done,
                },
                PlanStep {
                    text: "验证壁厚".to_string(),
                    status: StepStatus::InProgress,
                },
            ],
        ),
        Entry::check(13, "孔距 40 mm", true),
        Entry::error(14, "网格划分中断", "内存不足：需要 6.2 GB，可用 4.0 GB"),
        Entry::turn_end(15, "用时 34 秒 · 修改了 1 个特征"),
    ]
}

/// The answer the running session streams, long enough to be cut into rows.
fn long_answer() -> String {
    (1..=90)
        .map(|n| {
            format!(
                "### 第 {n} 步\n\n法兰的第 {n} 个螺栓孔位于节圆 **Ø120** 上，角度 `{}`°；\
                 这里的壁厚保持在最小值以上，不需要调整。\n\n",
                n * 4
            )
        })
        .collect()
}

impl Demo {
    fn new(id: &str, title: &str, status: SessionStatus) -> Self {
        Self {
            id: SessionId::new(id),
            title: title.to_string(),
            status,
            conversation: Rc::new(RefCell::new(Conversation::from_entries(showcase()))),
            plan: vec![
                PlanStep {
                    text: "读取顶面".to_string(),
                    status: StepStatus::Done,
                },
                PlanStep {
                    text: "生成孔阵列".to_string(),
                    status: StepStatus::InProgress,
                },
                PlanStep {
                    text: "验证壁厚".to_string(),
                    status: StepStatus::Pending,
                },
            ],
            plan_phase: PlanPhase::Executing,
        }
    }
}

impl WorkspaceView for Demo {
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
        self.conversation.borrow().clone()
    }
    fn plan(&self) -> Vec<PlanStep> {
        self.plan.clone()
    }
    fn plan_phase(&self) -> PlanPhase {
        self.plan_phase
    }
    fn clarify_questions(&self) -> Vec<String> {
        Vec::new()
    }
    fn progress(&self) -> Option<(u32, u32)> {
        None
    }
}

impl SessionView for Demo {
    type Scene = ();
    type Viewport = DemoViewport;
    type Workspace = Demo;

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
        self.conversation.borrow().clone()
    }
    fn plan(&self) -> Vec<PlanStep> {
        self.plan.clone()
    }
    fn plan_phase(&self) -> PlanPhase {
        self.plan_phase
    }
    fn clarify_questions(&self) -> Vec<String> {
        Vec::new()
    }
    fn branch_marker(&self) -> Option<BranchMarker> {
        None
    }
    fn build_workspace(&self, _viewport: Rc<RefCell<Viewport<Scene>>>) -> Demo {
        self.clone()
    }
}

fn store(streaming: Rc<RefCell<Conversation>>) -> SessionStore<Demo> {
    SessionStore::new(
        vec![
            Project {
                id: ProjectId::new("bracket"),
                name: "支架".to_string(),
                sessions: vec![
                    Demo::new("s1", "支架开孔方案", SessionStatus::Idle),
                    Demo::new("s2", "倒角待确认", SessionStatus::AwaitingUser),
                    Demo {
                        conversation: streaming,
                        ..Demo::new("s3", "法兰初稿（流式）", SessionStatus::Running)
                    },
                    Demo::new("s5", "壁厚检查出错", SessionStatus::Error),
                    Demo::new("s4", "加强筋待确认", SessionStatus::AwaitingUser),
                ],
                session_index: Some(SessionId::new("s1")),
            },
            Project {
                id: ProjectId::new("housing"),
                name: "壳体".to_string(),
                sessions: vec![Demo::new("h1", "壳体抽壳", SessionStatus::Idle)],
                session_index: Some(SessionId::new("h1")),
            },
        ],
        ProjectId::new("bracket"),
    )
}

fn main() {
    let config = nanobot_boot::settings::user_config_dir("shell_demo");
    nanobot_boot::diagnostics::init(&config, "nanobot-ui shell demo");
    let (settings, note) =
        nanobot_boot::Settings::load(&config.join(nanobot_boot::SETTINGS_FILE));
    if let Some(note) = note {
        log::warn!("{note}");
    }
    let settings = Arc::new(Mutex::new(settings));
    let streaming = Rc::new(RefCell::new(Conversation::default()));

    // The shell's icons are served by its own asset source.
    let app = gpui_platform::application().with_assets(nanobot_ui::Assets::new());
    app.run(move |cx: &mut App| {
        gpui_component::init(cx);
        nanobot_ui::init(cx);
        let bounds = Bounds::centered(
            None,
            Size {
                width: px(1500.),
                height: px(820.),
            },
            cx,
        );
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        cx.open_window(options, |window, cx| {
            cx.new(|cx| {
                let mut shell = HarnessShell::with_composer(
                    store(streaming.clone()),
                    nanobot_ui::Locale::default(),
                    window,
                    cx,
                );
                // The running session's answer arrives a little every tick,
                // ends, rests, and starts over.
                let (answer, sent) = (long_answer(), std::cell::Cell::new(0usize));
                let stream = streaming.clone();
                nanobot_ui::poll_periodically(
                    cx,
                    std::time::Duration::from_millis(40),
                    move |_, cx| {
                        let total = answer.chars().count();
                        let now = sent.get() + 12;
                        sent.set(if now > total + 12 * 150 { 0 } else { now });
                        let text: String = answer.chars().take(now.min(total)).collect();
                        let mut entries = vec![Entry::user(1, "给法兰布置螺栓孔，逐个说明。")];
                        if now >= total {
                            entries.push(Entry::assistant(2, text));
                            entries.push(Entry::turn_end(3, "用时 15 秒"));
                        } else {
                            entries.push(Entry::assistant_streaming(2, text));
                        }
                        stream.borrow_mut().entries = entries;
                        cx.notify();
                    },
                );
                shell.set_product_name("Shell demo");
                shell.set_settings(Some(settings.clone()));
                shell.panel_host().register(PanelDef::product(
                    "demo.properties",
                    "属性",
                    PanelArea::Right,
                    0,
                    Rc::new(|_shell, workspace: &Demo, _window, _cx| {
                        div()
                            .p_2()
                            .text_sm()
                            .child(format!("会话：{}", workspace.title))
                            .into_any_element()
                    }),
                ));
                shell
            })
        })
        .expect("a window");
        cx.activate(true);
    });
}
