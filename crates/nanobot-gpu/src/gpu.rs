//! The wgpu device every renderer in the process draws on.
//!
//! On the web, GPUI's renderer is wgpu itself, and a viewport can only reach
//! the screen as a texture GPUI samples (`WgpuDynamicTexture`), which has to
//! live on GPUI's own device: so the device is the one the web platform hands
//! out. On the desktop GPUI renders with Direct3D 11 (Windows) or Metal
//! (macOS) — there is no wgpu device to share — so the slot creates one: DX12
//! on Windows, where frames go to a swap chain on a DirectComposition visual
//! inside GPUI's window, and the platform's primary backend elsewhere, where
//! frames are read back.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::Window;

/// Extra device features and limits a product's renderer needs, beyond the
/// slot's baseline.
///
/// A CAD renderer's GPU-driven cull compute binds 5 storage buffers, say; an
/// undeclared shortfall makes every such pipeline fail validation and the view
/// silently shows the clear color. The host hands this to
/// [`set_gpu_requirements`] before the first viewport paints. On the web the
/// device is GPUI's and already exists, so the features are checked against it
/// instead and a shortfall is reported.
#[derive(Clone, Debug, Default)]
pub struct GpuRequirements {
    /// Features OR-ed onto the baseline (none).
    pub features: wgpu::Features,
    /// Limits merged with the baseline: upper bounds take the larger value,
    /// alignments and other lower bounds the smaller one.
    pub limits: Option<wgpu::Limits>,
}

impl GpuRequirements {
    /// The limits to request: `base` raised (or tightened) to what the
    /// product asked for.
    #[cfg(not(target_family = "wasm"))]
    fn merged_limits(&self, base: wgpu::Limits) -> wgpu::Limits {
        match &self.limits {
            Some(extra) => base.or_better_values_from(extra),
            None => base,
        }
    }
}

thread_local! {
    static REQUIREMENTS: RefCell<GpuRequirements> = RefCell::new(GpuRequirements::default());
    static SHARED_GPU: RefCell<GpuSlot> = const { RefCell::new(GpuSlot::NotTried) };
}

/// Records what the product needs from the device. Takes effect only before
/// the device is created (the first viewport to paint creates it).
pub fn set_gpu_requirements(requirements: GpuRequirements) {
    REQUIREMENTS.with(|r| *r.borrow_mut() = requirements);
}

fn requirements() -> GpuRequirements {
    REQUIREMENTS.with(|r| r.borrow().clone())
}

/// The one wgpu device/queue every renderer in the process draws on.
///
/// Product renderers and plugin views obtain this through [`shared_gpu`] and
/// build their own pipelines and textures on it. Frames are rendered in
/// [`Gpu::target_format`].
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    /// The color format every viewport frame is rendered in.
    pub target_format: wgpu::TextureFormat,
    /// The device's `max_texture_dimension_2d`.
    pub(crate) max_texture: u32,
    /// Windows: the instance and adapter the device came from, which
    /// composition swap chains are made with.
    #[cfg(target_os = "windows")]
    pub(crate) instance: wgpu::Instance,
    #[cfg(target_os = "windows")]
    pub(crate) adapter: wgpu::Adapter,
    /// Web: GPUI's context, which dynamic textures are allocated from.
    #[cfg(target_family = "wasm")]
    pub(crate) context: gpui_wgpu::WgpuContext,
}

impl Gpu {
    /// Web: GPUI's own device, from the platform's wgpu context.
    #[cfg(target_family = "wasm")]
    fn from_web_context(context: gpui_wgpu::WgpuContext) -> Self {
        let missing = requirements().features - context.device.features();
        if !missing.is_empty() {
            eprintln!("nanobot-gpu: GPUI's web device lacks requested features: {missing:?}");
        }
        Self {
            device: (*context.device).clone(),
            queue: (*context.queue).clone(),
            target_format: context.color_texture_format(),
            max_texture: context.device.limits().max_texture_dimension_2d,
            context,
        }
    }

    /// Desktop: a device of the slot's own.
    #[cfg(not(target_family = "wasm"))]
    fn new() -> anyhow::Result<Self> {
        use anyhow::Context as _;

        // Windows: frames go to a DirectComposition visual, which only the
        // DX12 backend can present to.
        #[cfg(target_os = "windows")]
        let backends = wgpu::Backends::DX12;
        #[cfg(not(target_os = "windows"))]
        let backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            flags: wgpu::InstanceFlags::default(),
            backend_options: wgpu::BackendOptions::default(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            display: None,
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .context("no suitable GPU adapter found")?;

        let wanted = requirements();
        let limits =
            wanted.merged_limits(wgpu::Limits::default().using_resolution(adapter.limits()));
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("nanobot-gpu-device"),
            required_features: wanted.features,
            required_limits: limits,
            ..Default::default()
        }))
        .context("failed to open the wgpu device")?;
        // Device errors are dispatched asynchronously; wgpu's default handler
        // panics, which would take the UI thread down with it.
        device.on_uncaptured_error(std::sync::Arc::new(|error: wgpu::Error| {
            eprintln!("nanobot-gpu: wgpu error: {error}");
        }));

        Ok(Self {
            max_texture: device.limits().max_texture_dimension_2d,
            device,
            queue,
            // Matches GPUI's own (non-sRGB) BGRA targets, so colors written by
            // a renderer come out as GPUI would show them; also the readback
            // path's byte order.
            target_format: wgpu::TextureFormat::Bgra8Unorm,
            #[cfg(target_os = "windows")]
            instance,
            #[cfg(target_os = "windows")]
            adapter,
        })
    }
}

/// Shared GPU slot. Rendering happens on the gpui thread, so a thread-local is
/// enough.
enum GpuSlot {
    /// Not created yet (web: graphics not up yet, retried on the next frame).
    NotTried,
    Ready(Rc<Gpu>),
    /// Device creation failed; not retried, so it does not block every frame.
    #[cfg(not(target_family = "wasm"))]
    Failed,
}

#[cfg(target_family = "wasm")]
thread_local! {
    /// The web platform the application runs on: it holds GPUI's wgpu
    /// context, which the viewports render on.
    static WEB_PLATFORM: RefCell<Option<Rc<gpui_web::WebPlatform>>> = const { RefCell::new(None) };
}

/// Web: remembers the platform the application was built on, whose wgpu
/// context [`shared_gpu`] hands out. The host calls this once at startup.
#[cfg(target_family = "wasm")]
pub fn set_web_platform(platform: Rc<gpui_web::WebPlatform>) {
    WEB_PLATFORM.with(|slot| *slot.borrow_mut() = Some(platform));
}

/// Acquires the process-wide GPU context, creating it on first use.
///
/// A renderer builds its own pipelines and textures on the returned device.
/// On the web this is GPUI's own device (so its textures can be sampled by
/// GPUI); on the desktop it is a device of the slot's own.
///
/// Returns `None` until the device is available (web: until graphics are up)
/// or when no adapter exists; callers skip the frame and retry on a later one.
pub fn shared_gpu(_window: &Window) -> Option<Rc<Gpu>> {
    SHARED_GPU.with(|cell| {
        let mut slot = cell.borrow_mut();
        if matches!(&*slot, GpuSlot::NotTried) {
            #[cfg(target_family = "wasm")]
            {
                let context = WEB_PLATFORM
                    .with(|platform| platform.borrow().as_ref().and_then(|p| p.wgpu_context()));
                if let Some(context) = context {
                    *slot = GpuSlot::Ready(Rc::new(Gpu::from_web_context(context)));
                }
            }
            #[cfg(not(target_family = "wasm"))]
            {
                match Gpu::new() {
                    Ok(gpu) => *slot = GpuSlot::Ready(Rc::new(gpu)),
                    Err(error) => {
                        eprintln!("nanobot-gpu: failed to init the wgpu device: {error:#}");
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

#[cfg(all(test, not(target_family = "wasm")))]
mod tests {
    use super::*;

    #[test]
    fn requirements_raise_the_baseline_limits() {
        let requirements = GpuRequirements {
            features: wgpu::Features::empty(),
            limits: Some(wgpu::Limits {
                max_storage_buffers_per_shader_stage: 16,
                ..wgpu::Limits::downlevel_defaults()
            }),
        };
        let base = wgpu::Limits::default();
        let merged = requirements.merged_limits(base.clone());
        // The renderer's larger bound wins; a smaller one does not lower the
        // baseline.
        assert_eq!(merged.max_storage_buffers_per_shader_stage, 16);
        assert_eq!(merged.max_texture_dimension_2d, base.max_texture_dimension_2d);
    }

    #[test]
    fn no_requirements_keep_the_baseline() {
        let base = wgpu::Limits::default();
        let merged = GpuRequirements::default().merged_limits(base.clone());
        assert_eq!(
            merged.max_storage_buffers_per_shader_stage,
            base.max_storage_buffers_per_shader_stage
        );
    }
}
