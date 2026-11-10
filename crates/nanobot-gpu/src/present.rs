//! How a viewport's frames reach the screen, and the shell every viewport
//! shares.
//!
//! GPUI's `Element::paint` only records primitives; it never hands out a
//! render pass. So a viewport renders with wgpu itself and gives GPUI the
//! result in whatever form the platform can take:
//!
//! * **Web** (`Presenter::Texture`): GPUI's renderer *is* wgpu. The viewport
//!   renders during paint into a `WgpuDynamicTexture` on GPUI's own device and
//!   paints it as a sprite, so it joins GPUI's painter order like any element.
//! * **Windows** (`Presenter::Native`): GPUI renders with Direct3D 11. The
//!   viewport gets a native surface in GPUI's window composition — a
//!   DirectComposition visual between GPUI's base content and its overlays
//!   (menus, popovers, dialogs, anything `deferred`) — and presents a DX12
//!   swap chain of its own there. Frames never leave the GPU, and input-driven
//!   frames are presented without a GPUI frame at all.
//! * **Elsewhere, or if composition fails** (`Presenter::Readback`): render
//!   offscreen, read the pixels back and paint them as an image.
//!
//! [`Viewport`] wraps a presenter with what every viewport needs — the dirty
//! flag and input routing — so a renderer only supplies its
//! [`ViewportContent`]: what the input does to its camera, and what it draws
//! into a color view in [`Gpu::target_format`].
//!
//! Ported from `workbench-ui/src/gpu_view/present.rs`.

use std::cell::RefCell;
use std::rc::{Rc, Weak};
#[cfg(not(target_family = "wasm"))]
use std::sync::Arc;

use anyhow::Result;
use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, ScrollDelta, ScrollEvent, Style, Window,
};

#[cfg(not(target_family = "wasm"))]
use crate::clamped_device_size;
use crate::{shared_gpu, Gpu, ViewportInput};

/// Offscreen targets (texture and readback paths) are capped at this many
/// device pixels per side.
const MAX_OFFSCREEN: u32 = 4096;

/// What a viewport shows. [`Viewport`] does the rest.
pub trait ViewportContent: 'static {
    /// Pointer or wheel input over the viewport. True when the view changed
    /// and needs a new frame.
    fn input(&mut self, input: ViewportInput) -> bool;
    /// Renders a frame into `view` ([`Gpu::target_format`], `size` in device
    /// pixels) on the [`shared_gpu`] device, submitting before it returns.
    fn draw(&mut self, gpu: &Gpu, view: &wgpu::TextureView, size: (u32, u32));
}

/// Wheel travel in pixels; a line counts like a wheel notch.
fn scroll_pixels(delta: ScrollDelta) -> f32 {
    match delta {
        ScrollDelta::Pixels(p) => f32::from(p.y),
        ScrollDelta::Lines(p) => p.y * 40.0,
    }
}

thread_local! {
    /// See [`suspend_viewport_input`].
    static INPUT_SUSPENDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// While suspended, no viewport takes pointer or wheel input, and over a
/// native surface the events go to GPUI's own listeners instead.
///
/// The shell turns this on for the length of a divider drag: the pointer
/// crosses the viewport while a neighbouring region is resized, and on Windows
/// the native surface would otherwise swallow the moves and the release, so
/// the drag would stall at the viewport's edge. Turn it off when the drag ends.
pub fn suspend_viewport_input(suspended: bool) {
    INPUT_SUSPENDED.with(|flag| flag.set(suspended));
}

fn input_suspended() -> bool {
    INPUT_SUSPENDED.with(std::cell::Cell::get)
}

/// What a native surface calls back into outside GPUI's frames.
trait ViewportOwner {
    /// True when the view changed and needs a new frame.
    fn feed(&mut self, input: ViewportInput) -> bool;
    /// Renders a frame now.
    #[cfg(target_os = "windows")]
    fn redraw_native(&mut self);
}

/// A viewport: its content, how its frames reach the screen, and its input.
///
/// A renderer keeps one per view in an `Rc<RefCell<_>>` and mounts it with
/// [`viewport_element`].
pub struct Viewport<C> {
    /// The renderer's own state.
    pub content: C,
    /// Chosen at the first prepaint that has a device.
    presenter: Option<Presenter>,
    dirty: bool,
    /// A left-button press began over the viewport and has not been released.
    pressed: bool,
}

impl<C: ViewportContent> Viewport<C> {
    pub fn new(content: C) -> Self {
        Self {
            content,
            presenter: None,
            dirty: true,
            pressed: false,
        }
    }

    /// The content changed (a new scene, say): the next paint renders a new
    /// frame.
    pub fn invalidate(&mut self) {
        self.dirty = true;
    }

    /// Prepaint: pick the way to the screen on first use and place a native
    /// surface where GPUI laid the element out (this must run in prepaint,
    /// which is what stacks the surface with its host). Returns the element's
    /// hitbox.
    fn prepaint(
        this: &Rc<RefCell<Self>>,
        gpu: &Rc<Gpu>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        let weak = Rc::downgrade(this);
        let owner: Weak<RefCell<dyn ViewportOwner>> = weak;
        let mut guard = this.borrow_mut();
        let viewport = &mut *guard;
        let presenter = viewport
            .presenter
            .get_or_insert_with(|| Presenter::new(gpu, owner, window, cx));
        if presenter.place(bounds, window.scale_factor()) {
            viewport.dirty = true;
        }
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    /// Paint: render if needed and show the frame.
    fn paint(
        this: &Rc<RefCell<Self>>,
        id: &GlobalElementId,
        bounds: Bounds<Pixels>,
        hitbox: &Hitbox,
        window: &mut Window,
    ) {
        let native_input = {
            let mut guard = this.borrow_mut();
            let Self {
                content,
                presenter,
                dirty,
                ..
            } = &mut *guard;
            let Some(presenter) = presenter else {
                return;
            };
            let dirty = std::mem::take(dirty);
            let gpu = presenter.gpu().clone();
            let mut draw = |view: &wgpu::TextureView, size| content.draw(&gpu, view, size);
            if let Err(error) = presenter.paint(id, bounds, dirty, window, &mut draw) {
                log::error!("{error:#}");
                return;
            }
            presenter.native_input()
        };
        // Over a native surface GPUI skips the bubble phase; input arrives
        // through the surface instead.
        if !native_input {
            listen_pointer(window, hitbox, this.clone());
        }
    }
}

impl<C: ViewportContent> ViewportOwner for Viewport<C> {
    fn feed(&mut self, input: ViewportInput) -> bool {
        if input_suspended() {
            return false;
        }
        let changed = self.content.input(input);
        self.dirty |= changed;
        changed
    }

    #[cfg(target_os = "windows")]
    fn redraw_native(&mut self) {
        let Self {
            content,
            presenter,
            dirty,
            ..
        } = self;
        if let Some(presenter) = presenter {
            let gpu = presenter.gpu().clone();
            presenter.redraw_native(&mut |view, size| content.draw(&gpu, view, size));
            *dirty = false;
        }
    }
}

/// Feeds a viewport's pointer input from ordinary GPUI listeners (the texture
/// and readback paths), asking the hosting view for a new frame when the view
/// changed. Registered during paint, as GPUI clears listeners every frame.
fn listen_pointer<C: ViewportContent>(
    window: &mut Window,
    hitbox: &Hitbox,
    viewport: Rc<RefCell<Viewport<C>>>,
) {
    let view = window.current_view();
    window.on_mouse_event({
        let (viewport, hitbox) = (viewport.clone(), hitbox.clone());
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble
                && event.button == MouseButton::Left
                && hitbox.is_hovered(window)
            {
                window.capture_pointer(hitbox.id);
                let mut viewport = viewport.borrow_mut();
                viewport.pressed = true;
                if viewport.feed(ViewportInput {
                    mouse_pos: Some(event.position),
                    mouse_down: true,
                    shift: event.modifiers.shift,
                    scroll: None,
                }) {
                    cx.notify(view);
                }
            }
        }
    });
    window.on_mouse_event({
        let (viewport, hitbox) = (viewport.clone(), hitbox.clone());
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let mut viewport = viewport.borrow_mut();
            // A drag that began here keeps feeding the viewport wherever the
            // pointer goes; otherwise only moves over it count.
            let dragging = viewport.pressed && event.dragging();
            if !dragging {
                // The release happened elsewhere.
                viewport.pressed = false;
            }
            if (dragging || hitbox.is_hovered(window))
                && viewport.feed(ViewportInput {
                    mouse_pos: Some(event.position),
                    mouse_down: dragging,
                    shift: event.modifiers.shift,
                    scroll: None,
                })
            {
                cx.notify(view);
            }
        }
    });
    window.on_mouse_event({
        let viewport = viewport.clone();
        move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                let mut viewport = viewport.borrow_mut();
                if std::mem::take(&mut viewport.pressed)
                    && viewport.feed(ViewportInput {
                        mouse_pos: Some(event.position),
                        mouse_down: false,
                        shift: event.modifiers.shift,
                        scroll: None,
                    })
                {
                    cx.notify(view);
                }
            }
        }
    });
    let hitbox = hitbox.clone();
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble
            && hitbox.is_hovered(window)
            && viewport.borrow_mut().feed(ViewportInput {
                mouse_pos: Some(event.position),
                mouse_down: false,
                shift: event.modifiers.shift,
                scroll: Some(scroll_pixels(event.delta)),
            })
        {
            cx.notify(view);
        }
    });
}

// ---------------------------------------------------------------------------
// The element.

/// The gpui element of a [`Viewport`]: full size, placed in prepaint, drawn in
/// paint. This is what a renderer returns from `ViewportSlot::element`.
pub fn viewport_element<C: ViewportContent>(viewport: &Rc<RefCell<Viewport<C>>>) -> AnyElement {
    ViewportElement {
        viewport: viewport.clone(),
    }
    .into_any_element()
}

struct ViewportElement<C> {
    viewport: Rc<RefCell<Viewport<C>>>,
}

impl<C: ViewportContent> IntoElement for ViewportElement<C> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<C: ViewportContent> Element for ViewportElement<C> {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        // Element state keeps a native surface's liveness token.
        Some("nanobot-gpu-viewport".into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Full size: an auto-sized custom element gets zero height in a flex
        // container and paints nothing.
        let style = Style {
            size: gpui::Size::full(),
            ..Default::default()
        };
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        match shared_gpu(window) {
            Some(gpu) => Viewport::prepaint(&self.viewport, &gpu, bounds, window, cx),
            // No device (yet): nothing is shown, and a later frame retries.
            None => window.insert_hitbox(bounds, HitboxBehavior::Normal),
        }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let id = id.expect("the element has an id");
        Viewport::paint(&self.viewport, id, bounds, hitbox, window);
    }
}

// ---------------------------------------------------------------------------
// Presenters.

/// One viewport's way to the screen. See the module docs.
enum Presenter {
    #[cfg(target_family = "wasm")]
    Texture(texture::TexturePresenter),
    #[cfg(target_os = "windows")]
    Native(native::NativePresenter),
    #[cfg(not(target_family = "wasm"))]
    Readback(ReadbackPresenter),
}

#[allow(unused_variables)]
impl Presenter {
    fn new(
        gpu: &Rc<Gpu>,
        owner: Weak<RefCell<dyn ViewportOwner>>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        #[cfg(target_family = "wasm")]
        let presenter = Self::Texture(texture::TexturePresenter::new(gpu.clone()));
        #[cfg(target_os = "windows")]
        let presenter = match native::NativePresenter::new(gpu.clone(), owner, window, cx) {
            Ok(presenter) => Self::Native(presenter),
            Err(error) => {
                log::warn!(
                    "window composition unavailable, reading frames back instead: {error:#}"
                );
                Self::Readback(ReadbackPresenter::new(gpu.clone()))
            }
        };
        #[cfg(not(any(target_family = "wasm", target_os = "windows")))]
        let presenter = Self::Readback(ReadbackPresenter::new(gpu.clone()));
        presenter
    }

    fn gpu(&self) -> &Rc<Gpu> {
        match self {
            #[cfg(target_family = "wasm")]
            Self::Texture(presenter) => presenter.gpu(),
            #[cfg(target_os = "windows")]
            Self::Native(presenter) => presenter.gpu(),
            #[cfg(not(target_family = "wasm"))]
            Self::Readback(presenter) => &presenter.gpu,
        }
    }

    /// Whether pointer input arrives through a native surface rather than
    /// GPUI listeners.
    fn native_input(&self) -> bool {
        match self {
            #[cfg(target_os = "windows")]
            Self::Native(_) => true,
            _ => false,
        }
    }

    /// Places a native surface; true when it was resized and needs a frame.
    fn place(&mut self, bounds: Bounds<Pixels>, scale: f32) -> bool {
        match self {
            #[cfg(target_os = "windows")]
            Self::Native(presenter) => presenter.place(bounds, scale).unwrap_or_else(|error| {
                log::error!("placing the native surface: {error:#}");
                false
            }),
            _ => false,
        }
    }

    /// Renders a frame if `dirty` (or the target had to change) and shows the
    /// current one.
    fn paint(
        &mut self,
        id: &GlobalElementId,
        bounds: Bounds<Pixels>,
        dirty: bool,
        window: &mut Window,
        draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32)),
    ) -> Result<()> {
        match self {
            #[cfg(target_family = "wasm")]
            Self::Texture(presenter) => presenter.paint(bounds, dirty, window, draw),
            #[cfg(target_os = "windows")]
            Self::Native(presenter) => {
                presenter.paint(id, dirty, window, draw);
                Ok(())
            }
            #[cfg(not(target_family = "wasm"))]
            Self::Readback(presenter) => presenter.paint(bounds, dirty, window, draw),
        }
    }

    /// Native surfaces render outside GPUI's frames too.
    #[cfg(target_os = "windows")]
    fn redraw_native(&mut self, draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32))) {
        if let Self::Native(presenter) = self {
            presenter.render(draw);
        }
    }
}

// ---------------------------------------------------------------------------
// Readback: offscreen render, CPU readback, `paint_image`.

/// An offscreen color target that can be read back: the color texture and an
/// aligned readback buffer.
#[cfg(not(target_family = "wasm"))]
struct ReadbackTarget {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    buffer: wgpu::Buffer,
    size: (u32, u32),
    bytes_per_row: u32,
}

#[cfg(not(target_family = "wasm"))]
impl ReadbackTarget {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat, size: (u32, u32)) -> Self {
        let (width, height) = size;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("nanobot-gpu-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let bytes_per_row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("nanobot-gpu-readback"),
            size: bytes_per_row as u64 * height as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let view = texture.create_view(&Default::default());
        Self {
            texture,
            view,
            buffer,
            size,
            bytes_per_row,
        }
    }

    /// Copies the frame out and blocks until done: tightly packed BGRA rows
    /// (`width * height * 4`, row padding removed).
    fn read(&self, device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Vec<u8>> {
        let (width, height) = self.size;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("nanobot-gpu-readback"),
        });
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.bytes_per_row),
                    rows_per_image: None,
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        let (sender, receiver) = std::sync::mpsc::channel();
        let slice = self.buffer.slice(..);
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: None,
            })
            .ok()?;
        receiver.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let row = width as usize * 4;
        let mut out = Vec::with_capacity(row * height as usize);
        for chunk in data.chunks(self.bytes_per_row as usize).take(height as usize) {
            out.extend_from_slice(&chunk[..row]);
        }
        drop(data);
        self.buffer.unmap();
        Some(out)
    }
}

/// Renders offscreen and paints the pixels as an image. Every desktop can do
/// this; it costs a GPU→CPU copy and an atlas upload per frame.
#[cfg(not(target_family = "wasm"))]
struct ReadbackPresenter {
    gpu: Rc<Gpu>,
    target: Option<ReadbackTarget>,
    image: Option<Arc<gpui::RenderImage>>,
}

#[cfg(not(target_family = "wasm"))]
impl ReadbackPresenter {
    fn new(gpu: Rc<Gpu>) -> Self {
        Self {
            gpu,
            target: None,
            image: None,
        }
    }

    fn paint(
        &mut self,
        bounds: Bounds<Pixels>,
        dirty: bool,
        window: &mut Window,
        draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32)),
    ) -> Result<()> {
        let gpu = &self.gpu;
        let size = clamped_device_size(
            &bounds,
            window.scale_factor(),
            gpu.max_texture.min(MAX_OFFSCREEN),
        );
        let resized = self.target.as_ref().is_none_or(|t| t.size != size);
        if resized {
            self.target = Some(ReadbackTarget::new(&gpu.device, gpu.target_format, size));
        }
        if resized || dirty || self.image.is_none() {
            let target = self.target.as_ref().expect("allocated above");
            draw(&target.view, size);
            let bytes = target
                .read(&gpu.device, &gpu.queue)
                .ok_or_else(|| anyhow::anyhow!("reading the frame back failed"))?;
            let frame = image::Frame::new(
                image::ImageBuffer::from_raw(size.0, size.1, bytes)
                    .ok_or_else(|| anyhow::anyhow!("readback size mismatch"))?,
            );
            let image = Arc::new(gpui::RenderImage::new(vec![frame]));
            // Each frame is a new image; drop the last one from the sprite
            // atlas, or every frame would stay there.
            if let Some(previous) = self.image.replace(image) {
                window.drop_image(previous).ok();
            }
        }
        let image = self.image.clone().expect("rendered above");
        window.paint_image(bounds, bounds, gpui::Corners::all(gpui::px(0.)), image, 0, false)
    }
}

// ---------------------------------------------------------------------------
// Web: a dynamic texture on GPUI's device.

#[cfg(target_family = "wasm")]
mod texture {
    use std::rc::Rc;

    use anyhow::Result;
    use gpui::{Bounds, DevicePixels, Pixels, Window, size};
    use gpui_wgpu::WgpuDynamicTexture;

    use super::{Gpu, MAX_OFFSCREEN};
    use crate::clamped_device_size;

    /// Renders during paint into a texture GPUI samples as a sprite.
    pub(super) struct TexturePresenter {
        gpu: Rc<Gpu>,
        texture: Option<(WgpuDynamicTexture, (u32, u32))>,
    }

    impl TexturePresenter {
        pub(super) fn new(gpu: Rc<Gpu>) -> Self {
            Self { gpu, texture: None }
        }

        pub(super) fn gpu(&self) -> &Rc<Gpu> {
            &self.gpu
        }

        pub(super) fn paint(
            &mut self,
            bounds: Bounds<Pixels>,
            dirty: bool,
            window: &mut Window,
            draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32)),
        ) -> Result<()> {
            let max = self.gpu.max_texture.min(MAX_OFFSCREEN);
            let target = clamped_device_size(&bounds, window.scale_factor(), max);
            let resized = self
                .texture
                .as_ref()
                .is_none_or(|(_, allocated)| *allocated != target);
            if resized {
                let texture = self.gpu.context.create_dynamic_texture(size(
                    DevicePixels(target.0 as i32),
                    DevicePixels(target.1 as i32),
                ))?;
                self.texture = Some((texture, target));
            }
            let (texture, _) = self.texture.as_ref().expect("allocated above");
            if resized || dirty {
                // Submits on GPUI's queue before GPUI's own submission samples
                // the texture, so no wait or copy is needed.
                texture.render(window, |view| {
                    draw(view, target);
                    Ok(())
                })?;
            }
            texture.paint(window, bounds)
        }
    }
}

// ---------------------------------------------------------------------------
// Windows: a DX12 swap chain on a DirectComposition visual.

#[cfg(target_os = "windows")]
mod native {
    use std::cell::{Cell, RefCell};
    use std::rc::{Rc, Weak};
    use std::time::{Duration, Instant};

    use anyhow::{Context as _, Result, anyhow};
    use gpui::{
        App, BackgroundExecutor, Bounds, DevicePixels, DispatchEventResult, ForegroundExecutor,
        GlobalElementId, MouseButton, Pixels, PlatformInput, Point, Window,
        WindowCompositionSurface, WindowId,
    };
    use windows::Win32::Graphics::DirectComposition::IDCompositionVisual;
    use windows::core::{IUnknown, Interface as _};

    use super::{Gpu, ViewportOwner, scroll_pixels};
    use crate::ViewportInput;

    /// Input-driven frames are paced to about this interval. A mouse posts
    /// events far faster than a display shows frames, and Mailbox presents
    /// never block to slow that down.
    const FRAME_INTERVAL: Duration = Duration::from_micros(8_333);

    thread_local! {
        /// Native surfaces of closed viewports, by window. gpui-fast cannot
        /// remove a surface on Windows (`remove_surface` hides it first, and
        /// hiding casts the visual to `IDCompositionVisual3`, which visuals
        /// of its `DCompositionCreateDevice` device don't implement:
        /// E_NOINTERFACE), so they are parked off screen and reused.
        static SPARE: RefCell<Vec<(WindowId, WindowCompositionSurface)>> =
            const { RefCell::new(Vec::new()) };
    }

    /// Whether this thread's locals are being torn down: at process exit the
    /// application can be dropped by thread-local destructors. Those run in
    /// reverse order of registration, and [`SPARE`] registers only when the
    /// first native surface is made — after every thread local that can own
    /// one — so it is gone by then. There is no window left to hide anything
    /// in, and touching a destroyed thread local would abort the process.
    fn tearing_down() -> bool {
        SPARE.try_with(|_| ()).is_err()
    }

    /// The surface's GPUI side and its swap chain, shared with the input
    /// handler, the redraw task and the frame-liveness tokens.
    struct Shared {
        gpu: Rc<Gpu>,
        window: WindowId,
        surface: WindowCompositionSurface,
        swap_chain: wgpu::Surface<'static>,
        config: RefCell<wgpu::SurfaceConfiguration>,
        /// Configured since the last present: GPUI must commit its
        /// DirectComposition device after the next one, as wgpu sets the
        /// swap chain as the visual's content but cannot commit a device it
        /// doesn't own.
        needs_commit: Cell<bool>,
        /// Whether the surface has its bounds (rather than empty ones).
        shown: Cell<bool>,
        /// Liveness tokens held by frames that include the element.
        live: Cell<usize>,
        /// A redraw task is queued.
        redraw_queued: Cell<bool>,
        last_present: Cell<Instant>,
        owner: Weak<RefCell<dyn ViewportOwner>>,
        executor: ForegroundExecutor,
        timers: BackgroundExecutor,
    }

    impl Shared {
        /// Hides the surface: GPUI neither shows nor routes input to empty
        /// bounds. (`set_visible` fails on Windows; see [`SPARE`].)
        fn hide(&self) {
            if self.shown.replace(false) {
                let hidden = self
                    .surface
                    .platform_surface()
                    .and_then(|surface| surface.set_bounds(Bounds::default()));
                if let Err(error) = hidden {
                    log::error!("hiding the native surface: {error:#}");
                }
            }
        }

        /// Makes GPUI commit its composition device. Setting the parent origin
        /// is the public call that commits even when nothing changed (an
        /// implementation detail of gpui-fast's Windows backend).
        fn commit(&self) {
            let committed = self
                .surface
                .platform_surface()
                .and_then(|surface| surface.set_parent_origin(Point::default()));
            if let Err(error) = committed {
                log::error!("committing the composition: {error:#}");
            }
        }

        /// Renders the owner's next frame, at most one per [`FRAME_INTERVAL`].
        fn queue_redraw(self: &Rc<Self>) {
            if self.redraw_queued.replace(true) {
                return;
            }
            let wait = FRAME_INTERVAL.saturating_sub(self.last_present.get().elapsed());
            let timer = self.timers.timer(wait);
            let shared = Rc::downgrade(self);
            self.executor
                .spawn(async move {
                    timer.await;
                    let Some(shared) = shared.upgrade() else {
                        return;
                    };
                    shared.redraw_queued.set(false);
                    if let Some(owner) = shared.owner.upgrade() {
                        owner.borrow_mut().redraw_native();
                    }
                })
                .detach();
        }

        /// Renders and presents one frame.
        fn render(&self, draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32))) {
            if !self.shown.get() {
                return;
            }
            let config = self.config.borrow();
            let frame = match self.swap_chain.get_current_texture() {
                wgpu::CurrentSurfaceTexture::Success(frame)
                | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
                wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                    self.swap_chain.configure(&self.gpu.device, &config);
                    self.needs_commit.set(true);
                    return;
                }
                // Occluded or timed out: the next change draws again.
                _ => return,
            };
            let view = frame
                .texture
                .create_view(&wgpu::TextureViewDescriptor::default());
            draw(&view, (config.width, config.height));
            self.gpu.queue.present(frame);
            self.last_present.set(Instant::now());
            if self.needs_commit.replace(false) {
                self.commit();
            }
        }
    }

    impl Drop for Shared {
        fn drop(&mut self) {
            if tearing_down() {
                return;
            }
            self.hide();
            let spare = (self.window, self.surface.clone());
            let _ = SPARE.try_with(|pool| pool.borrow_mut().push(spare));
        }
    }

    /// Kept as the painted element's state, which GPUI keeps for exactly as
    /// long as a frame includes the element: a frame that reuses a retained
    /// view carries it along, and the frame that replaces the last one
    /// including the element drops it. When the last token goes, the element
    /// is gone from the screen (its session was switched away, its panel
    /// closed) and the surface, which GPUI would otherwise leave where it
    /// was, is hidden.
    struct LiveToken(Rc<Shared>);

    impl LiveToken {
        fn new(shared: &Rc<Shared>) -> Self {
            shared.live.set(shared.live.get() + 1);
            Self(shared.clone())
        }
    }

    impl Drop for LiveToken {
        fn drop(&mut self) {
            let live = self.0.live.get() - 1;
            self.0.live.set(live);
            if live == 0 && !tearing_down() {
                // Dropped while GPUI finishes a frame: act once it is done.
                let shared = Rc::downgrade(&self.0);
                self.0
                    .executor
                    .spawn(async move {
                        if let Some(shared) = shared.upgrade() {
                            if shared.live.get() == 0 {
                                shared.hide();
                            }
                        }
                    })
                    .detach();
            }
        }
    }

    pub(super) struct NativePresenter {
        shared: Rc<Shared>,
    }

    impl NativePresenter {
        pub(super) fn new(
            gpu: Rc<Gpu>,
            owner: Weak<RefCell<dyn ViewportOwner>>,
            window: &mut Window,
            cx: &mut App,
        ) -> Result<Self> {
            let window_id = window.window_handle().window_id();
            let composition = window.enable_window_composition()?;
            let spare = SPARE.with(|pool| {
                let mut pool = pool.borrow_mut();
                let index = pool.iter().position(|(id, _)| *id == window_id)?;
                Some(pool.swap_remove(index).1)
            });
            let surface = match spare {
                Some(surface) => surface,
                None => composition.create_native_surface()?,
            };

            let handle = surface.platform_surface()?.platform_handle()?;
            let visual: IDCompositionVisual = handle
                .downcast::<IUnknown>()
                .map_err(|_| anyhow!("the native surface is not a DirectComposition visual"))?
                .cast()
                .context("casting to IDCompositionVisual")?;
            // SAFETY: wgpu takes its own reference to the visual and holds it
            // for as long as the swap chain lives.
            let swap_chain = unsafe {
                gpu.instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::CompositionVisual(
                    visual.as_raw(),
                ))
            }
            .context("creating a wgpu surface on the composition visual")?;
            let config = surface_config(&gpu, &swap_chain)?;
            swap_chain.configure(&gpu.device, &config);

            // Pointer input over the surface comes here rather than to GPUI
            // listeners, in window logical pixels.
            let input_owner = owner.clone();
            let shared = Rc::new(Shared {
                gpu,
                window: window_id,
                surface: surface.clone(),
                swap_chain,
                config: RefCell::new(config),
                needs_commit: Cell::new(true),
                shown: Cell::new(false),
                live: Cell::new(0),
                redraw_queued: Cell::new(false),
                last_present: Cell::new(Instant::now()),
                owner,
                executor: cx.foreground_executor().clone(),
                timers: cx.background_executor().clone(),
            });
            let weak = Rc::downgrade(&shared);
            composition.on_input(surface.id(), move |event| {
                // A divider drag in progress: let GPUI's listeners have it.
                if super::input_suspended() {
                    return DispatchEventResult {
                        propagate: true,
                        default_prevented: false,
                    };
                }
                let input = match event {
                    PlatformInput::MouseDown(event) if event.button == MouseButton::Left => {
                        ViewportInput {
                            mouse_pos: Some(event.position),
                            mouse_down: true,
                            shift: event.modifiers.shift,
                            scroll: None,
                        }
                    }
                    PlatformInput::MouseUp(event) if event.button == MouseButton::Left => {
                        ViewportInput {
                            mouse_pos: Some(event.position),
                            mouse_down: false,
                            shift: event.modifiers.shift,
                            scroll: None,
                        }
                    }
                    PlatformInput::MouseMove(event) => ViewportInput {
                        mouse_pos: Some(event.position),
                        mouse_down: event.dragging(),
                        shift: event.modifiers.shift,
                        scroll: None,
                    },
                    PlatformInput::Scroll(event) => ViewportInput {
                        mouse_pos: Some(event.position),
                        mouse_down: false,
                        shift: event.modifiers.shift,
                        scroll: Some(scroll_pixels(event.delta)),
                    },
                    _ => {
                        return DispatchEventResult {
                            propagate: true,
                            default_prevented: false,
                        };
                    }
                };
                let changed = input_owner
                    .upgrade()
                    .is_some_and(|owner| owner.borrow_mut().feed(input));
                if changed {
                    if let Some(shared) = weak.upgrade() {
                        shared.queue_redraw();
                    }
                }
                DispatchEventResult {
                    propagate: false,
                    default_prevented: false,
                }
            })?;

            Ok(Self { shared })
        }

        pub(super) fn gpu(&self) -> &Rc<Gpu> {
            &self.shared.gpu
        }

        /// Puts the surface where GPUI laid the element out, resizing the swap
        /// chain to match. True when it was resized.
        pub(super) fn place(&self, bounds: Bounds<Pixels>, scale: f32) -> Result<bool> {
            let shared = &self.shared;
            let device: Bounds<DevicePixels> = bounds.to_device_pixels(scale);
            if device.size.width.0 <= 0 || device.size.height.0 <= 0 {
                shared.hide();
                return Ok(false);
            }
            shared.surface.platform_surface()?.set_bounds(device)?;
            shared.shown.set(true);

            let max = shared.gpu.max_texture;
            let size = (
                (device.size.width.0 as u32).min(max),
                (device.size.height.0 as u32).min(max),
            );
            let mut config = shared.config.borrow_mut();
            if (config.width, config.height) == size {
                return Ok(false);
            }
            (config.width, config.height) = size;
            shared.swap_chain.configure(&shared.gpu.device, &config);
            shared.needs_commit.set(true);
            Ok(true)
        }

        pub(super) fn paint(
            &self,
            id: &GlobalElementId,
            dirty: bool,
            window: &mut Window,
            draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32)),
        ) {
            let shared = &self.shared;
            window.with_element_state(id, |token: Option<LiveToken>, _| {
                let token = token
                    .filter(|token| Rc::ptr_eq(&token.0, shared))
                    .unwrap_or_else(|| LiveToken::new(shared));
                ((), token)
            });
            if dirty {
                shared.render(draw);
            }
        }

        pub(super) fn render(&self, draw: &mut dyn FnMut(&wgpu::TextureView, (u32, u32))) {
            self.shared.render(draw);
        }
    }

    fn surface_config(
        gpu: &Gpu,
        swap_chain: &wgpu::Surface<'_>,
    ) -> Result<wgpu::SurfaceConfiguration> {
        let capabilities = swap_chain.get_capabilities(&gpu.adapter);
        anyhow::ensure!(
            capabilities.formats.contains(&gpu.target_format),
            "the composition swap chain cannot use {:?}",
            gpu.target_format
        );
        // Frames are presented on the UI thread, so don't wait for vblank:
        // Mailbox replaces a queued frame instead of blocking. DWM composes
        // the visual, so nothing tears either way.
        let present_mode = [wgpu::PresentMode::Mailbox, wgpu::PresentMode::Fifo]
            .into_iter()
            .find(|mode| capabilities.present_modes.contains(mode))
            .unwrap_or(wgpu::PresentMode::Fifo);
        Ok(wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: gpu.target_format,
            width: 1,
            height: 1,
            present_mode,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Opaque,
            color_space: wgpu::SurfaceColorSpace::Auto,
            view_formats: Vec::new(),
        })
    }
}
