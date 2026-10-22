//! nanobot-gpu: the harness GPU render slot (design 4.9).
//!
//! The slot owns the three things that are *independent of what is being
//! rendered*:
//!
//! 1. the process-wide shared wgpu device ([`shared_gpu`]) — taken from gpui's
//!    own renderer on Windows/Web, created standalone elsewhere;
//! 2. the offscreen color target ([`gpui_wgpu::WgpuRenderTarget`]) that a
//!    renderer draws into;
//! 3. the zero-copy composite ([`paint_surface`]) that blits that target into
//!    a gpui element's bounds.
//!
//! The *content* is the product's: `Mesh` / `Camera` / `SceneObjectUpload` /
//! `WgpuRenderer` live in the CAD renderer, and a plugin cannot build a wgpu
//! pipeline at all (scripts carry no code). What the product gets is the seam:
//!
//! ```text
//! nanobot_gpu::shared_gpu(window)          // the one device
//!   -> product renderer builds its pipelines on gpu.device() / gpu.queue()
//!   -> product renderer draws into its own offscreen target
//!   -> nanobot_gpu::paint_surface(window, &target, bounds)
//! ```
//!
//! Sharing the device is not optional. On Windows/Web `paint_surface` composites
//! a texture owned by gpui's renderer, so that texture must live on that
//! renderer's device; a self-built device cannot composite and the view silently
//! shows the clear color. This is the mechanism behind design 3.2 rule 5: the
//! shared device is the only wgpu entry point.
//!
//! The shape follows `workbench-ui/src/gpu_view/{mod,gpu}.rs`, with one
//! deliberate difference: the gpui context handle is stored as an `Option` on
//! every platform instead of being `cfg`-gated, so a renderer's code path does
//! not fork per platform.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{Bounds, Pixels, Window};

/// The one wgpu device/queue every renderer in the process draws on.
///
/// Product renderers and plugin views obtain this through [`shared_gpu`] and
/// build their own pipelines, textures and render targets on it.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// Surface format every offscreen target uses.
    pub target_format: wgpu::TextureFormat,
    /// gpui's renderer context (shared-device path). `None` on platforms where
    /// gpui does not own a wgpu device and the slot created a standalone one.
    handle: Option<gpui_wgpu::WgpuContextHandle>,
}

impl Gpu {
    /// The gpui renderer context handle, needed to build a
    /// [`gpui_wgpu::WgpuRenderTarget`]. `None` while the renderer is not up.
    pub fn context_handle(&self) -> Option<&gpui_wgpu::WgpuContextHandle> {
        self.handle.as_ref()
    }

    /// Takes the shared wgpu context from the gpui window.
    ///
    /// Returns `None` before the first frame (renderer not ready yet); callers
    /// retry on the next frame.
    fn from_window(window: &Window) -> Option<Self> {
        let handle = gpui_wgpu::WgpuContextHandle::from_window(window)?;
        // DX12 device errors are dispatched asynchronously; without a callback
        // they are silently dropped.
        handle
            .device()
            .on_uncaptured_error(std::sync::Arc::new(|error| {
                eprintln!("wgpu uncaptured error: {error}");
            }));
        Some(Self {
            device: handle.device().clone(),
            queue: handle.queue().clone(),
            target_format: handle.texture_format(),
            handle: Some(handle),
        })
    }

    /// Creates a standalone device (platforms where gpui's renderer is not
    /// wgpu-backed).
    #[cfg(not(any(target_family = "wasm", target_os = "windows")))]
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        }))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nanobot-gpu-device"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: Default::default(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        Some(Self {
            device,
            queue,
            target_format: wgpu::TextureFormat::Bgra8Unorm,
            handle: None,
        })
    }
}

/// Shared GPU slot. Rendering happens on the gpui thread, so a thread-local is
/// enough; `Failed` only applies to the standalone-device path, so a device
/// creation failure does not retry (and block) every frame.
enum GpuSlot {
    /// Windows/Web: the renderer is not ready yet, retry on the next frame.
    NotTried,
    Ready(Rc<Gpu>),
    #[cfg(not(any(target_family = "wasm", target_os = "windows")))]
    Failed,
}

thread_local! {
    static SHARED_GPU: RefCell<GpuSlot> = const { RefCell::new(GpuSlot::NotTried) };
}

/// Acquires the process-wide GPU context, initializing it from `window` on
/// first use.
///
/// A renderer's `Element::paint` calls this and builds its own pipelines,
/// textures and render targets on the returned device. Returns `None` until
/// gpui's renderer is ready; callers skip the frame and retry rather than
/// treating it as an error.
pub fn shared_gpu(window: &Window) -> Option<Rc<Gpu>> {
    SHARED_GPU.with(|cell| {
        let mut slot = cell.borrow_mut();
        if matches!(&*slot, GpuSlot::NotTried) {
            #[cfg(any(target_family = "wasm", target_os = "windows"))]
            {
                if let Some(gpu) = Gpu::from_window(window) {
                    *slot = GpuSlot::Ready(Rc::new(gpu));
                }
            }
            #[cfg(not(any(target_family = "wasm", target_os = "windows")))]
            {
                match Gpu::new() {
                    Some(gpu) => *slot = GpuSlot::Ready(Rc::new(gpu)),
                    None => {
                        eprintln!("nanobot-gpu: failed to init the wgpu device");
                        *slot = GpuSlot::Failed;
                    }
                }
            }
        }
        match &*slot {
            GpuSlot::Ready(gpu) => Some(gpu.clone()),
            _ => None,
        }
    })
}

/// One frame of zero-copy compositing: draws `target` into `bounds`.
///
/// `target` must have been created on the device behind [`shared_gpu`] (see
/// [`Gpu::context_handle`]); a target from another device composites to the
/// clear color rather than erroring.
pub fn paint_surface(
    window: &mut Window,
    target: &gpui_wgpu::WgpuRenderTarget,
    bounds: Bounds<Pixels>,
) {
    let source = gpui::SurfaceSource::Texture {
        texture: target.texture(),
        size: target.size(),
    };
    window.paint_surface(bounds, source);
}

/// Device requirements the slot needs from the host, on top of gpui's own
/// baseline.
///
/// `extra_limits` carries the renderer's own needs — a CAD renderer's GPU-driven
/// cull compute binds 5 storage buffers while gpui's device baseline
/// (`wgpu::Limits::downlevel_defaults()`) allows 4, and an undeclared shortfall
/// makes every cull pipeline fail validation so the view silently shows the
/// clear color. The host applies this before the first window opens
/// (`gpui::App::set_gpu_requirements`).
pub fn gpu_requirements(
    extra_limits: Option<wgpu::Limits>,
) -> gpui_wgpu::WgpuDeviceRequirements {
    gpui_wgpu::WgpuDeviceRequirements {
        features: wgpu::Features::empty(),
        limits: extra_limits,
    }
}

/// Device-pixel size of a paint bounds at the window's scale factor, clamped to
/// a sane maximum. Shared by every offscreen target so a resize is a plain
/// comparison.
pub fn device_size(bounds: &Bounds<Pixels>, scale_factor: f32) -> (u32, u32) {
    let width = (f32::from(bounds.size.width) * scale_factor).round().max(1.0) as u32;
    let height = (f32::from(bounds.size.height) * scale_factor).round().max(1.0) as u32;
    (width.min(4096), height.min(4096))
}

/// Aspect ratio of a device-pixel size, guarded against a zero height.
pub fn aspect(size: (u32, u32)) -> f32 {
    size.0.max(1) as f32 / size.1.max(1) as f32
}

// ---- the viewport slot contract ----

/// One frame of cursor / wheel input to a viewport, in logical pixels.
///
/// Drag = orbit, Shift+drag = pan, wheel = zoom; the mapping belongs to the
/// renderer, the shell only forwards the raw input.
#[derive(Default, Clone, Copy, Debug)]
pub struct ViewportInput {
    pub mouse_pos: Option<gpui::Point<Pixels>>,
    pub mouse_down: bool,
    pub shift: bool,
    pub scroll: Option<f32>,
}

/// The scene projection a renderer reports. The renderer owns the default
/// (the CAD viewport opens orthographic); the shell only displays it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewportProjection {
    Orthographic,
    Perspective,
}

/// What a renderer provides so the shell can host a viewport region.
///
/// The shell is generic over this: it forwards input, hands over a scene, asks
/// for the projection and the painted-frame count, and mounts the renderer's
/// own gpui element. Everything domain-specific (meshes, cameras, pipelines)
/// stays behind the associated types.
///
/// The methods are associated functions (no `&self`) because a renderer is a
/// marker type: the state lives in the per-session [`Self::Handle`].
pub trait ViewportSlot: 'static {
    /// The renderer's per-session handle (the shell keeps one per session).
    type Handle: Clone + 'static;
    /// The renderer's scene wire format.
    type Scene: 'static;

    fn new_handle() -> Self::Handle;
    fn feed_input(handle: &Self::Handle, input: ViewportInput);
    fn update_scene(handle: &Self::Handle, scene: &Self::Scene);
    fn projection(handle: &Self::Handle) -> Option<ViewportProjection>;
    fn painted_frames(handle: &Self::Handle) -> u64;
    /// The renderer's element, which paints through [`shared_gpu`] /
    /// [`paint_surface`] inside its own `paint`.
    fn element(handle: &Self::Handle) -> gpui::AnyElement;
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{DevicePixels, Size, size};

    #[test]
    fn input_defaults_to_idle() {
        let input = ViewportInput::default();
        assert!(input.mouse_pos.is_none());
        assert!(!input.mouse_down);
        assert!(!input.shift);
        assert!(input.scroll.is_none());
    }

    #[test]
    fn projections_are_distinguishable() {
        assert_ne!(
            ViewportProjection::Orthographic,
            ViewportProjection::Perspective
        );
    }

    #[test]
    fn device_size_scales_and_clamps() {
        let bounds = Bounds {
            origin: gpui::Point {
                x: gpui::px(0.0),
                y: gpui::px(0.0),
            },
            size: size(px(100.0), px(50.0)),
        };
        assert_eq!(device_size(&bounds, 1.0), (100, 50));
        assert_eq!(device_size(&bounds, 2.0), (200, 100));
        // A zero-sized bounds still yields a renderable target.
        let empty = Bounds {
            origin: gpui::Point {
                x: gpui::px(0.0),
                y: gpui::px(0.0),
            },
            size: size(px(0.0), px(0.0)),
        };
        assert_eq!(device_size(&empty, 1.0), (1, 1));
        // The clamp keeps a huge HiDPI window from allocating a 16k target.
        let huge = Bounds {
            origin: gpui::Point {
                x: gpui::px(0.0),
                y: gpui::px(0.0),
            },
            size: size(px(4096.0), px(4096.0)),
        };
        assert_eq!(device_size(&huge, 8.0), (4096, 4096));
    }

    #[test]
    fn aspect_guards_a_zero_height() {
        assert_eq!(aspect((100, 50)), 2.0);
        assert_eq!(aspect((100, 0)), 100.0);
        assert_eq!(aspect((0, 0)), 1.0);
    }

    #[test]
    fn requirements_carry_the_renderer_limits() {
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffers_per_shader_stage = 5;
        let requirements = gpu_requirements(Some(limits));
        let carried = requirements
            .limits
            .expect("the renderer declared limits");
        assert_eq!(carried.max_storage_buffers_per_shader_stage, 5);
        // No features are requested by the slot itself.
        assert_eq!(requirements.features, wgpu::Features::empty());
    }

    /// The size type the offscreen target is built with, pinned here so a
    /// gpui signature change surfaces in this crate's tests.
    #[test]
    fn target_size_type_is_device_pixels() {
        let size: Size<DevicePixels> = size(DevicePixels(640), DevicePixels(480));
        assert_eq!(size.width, DevicePixels(640));
    }

    fn px(v: f32) -> Pixels {
        gpui::px(v)
    }
}
