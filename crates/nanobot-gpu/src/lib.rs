//! nanobot-gpu: the harness GPU render slot (design 4.9).
//!
//! The slot owns the three things that are *independent of what is being
//! rendered*:
//!
//! 1. the process-wide wgpu device ([`shared_gpu`]) — GPUI's own on the web,
//!    one of the slot's own on the desktop;
//! 2. the way a frame reaches the screen, per platform ([`Viewport`]): a
//!    texture GPUI samples on the web, a swap chain on a DirectComposition
//!    surface inside GPUI's window on Windows, an offscreen render read back
//!    into an image elsewhere;
//! 3. input routing: pointer and wheel input over the viewport, whichever of
//!    those paths it arrives on, reaches the renderer as [`ViewportInput`].
//!
//! The *content* is the product's: `Mesh` / `Camera` / `SceneObjectUpload` /
//! `WgpuRenderer` live in the CAD renderer, and a plugin cannot build a wgpu
//! pipeline at all (scripts carry no code). What the product gets is the seam:
//!
//! ```text
//! impl nanobot_gpu::ViewportContent for ProductRenderer {
//!     fn input(&mut self, input) -> bool   // move the camera; true = redraw
//!     fn draw(&mut self, gpu, view, size)  // render into `view`, submit
//! }
//! let viewport = Rc::new(RefCell::new(nanobot_gpu::Viewport::new(renderer)));
//! nanobot_gpu::viewport_element(&viewport) // the gpui element to mount
//! ```
//!
//! Sharing the device is not optional. On the web the frame is a texture GPUI
//! samples, so it must live on GPUI's device; on Windows the swap chain is made
//! by the instance and adapter the device came from. A renderer that builds its
//! own device cannot draw into the view it is handed. This is the mechanism
//! behind design 3.2 rule 5: the shared device is the only wgpu entry point.
//!
//! GPUI itself is gpui-fast, which has no hook for drawing inside its render
//! pass, so the presentation follows `workbench-ui/src/gpu_view` (the
//! per-platform detail is in `present.rs`).

mod gpu;
mod present;

use gpui::{Bounds, DevicePixels, Pixels};

#[cfg(target_family = "wasm")]
pub use gpu::set_web_platform;
pub use gpu::{set_gpu_requirements, shared_gpu, Gpu, GpuRequirements};
pub use present::{suspend_viewport_input, viewport_element, Viewport, ViewportContent};

/// Device-pixel size of a paint bounds at the window's scale factor, at least
/// 1×1 and clamped to a sane maximum.
pub fn device_size(bounds: &Bounds<Pixels>, scale_factor: f32) -> (u32, u32) {
    clamped_device_size(bounds, scale_factor, 4096)
}

/// Device-pixel size of `bounds`, at least 1×1 and at most `max` per side.
/// Rounds the way GPUI places the element, so a frame covers it exactly.
fn clamped_device_size(bounds: &Bounds<Pixels>, scale_factor: f32, max: u32) -> (u32, u32) {
    let device: Bounds<DevicePixels> = bounds.to_device_pixels(scale_factor);
    (
        (device.size.width.0.max(1) as u32).min(max),
        (device.size.height.0.max(1) as u32).min(max),
    )
}

/// Aspect ratio of a device-pixel size, guarded against a zero height.
pub fn aspect(size: (u32, u32)) -> f32 {
    size.0.max(1) as f32 / size.1.max(1) as f32
}

// ---- the viewport slot contract ----

/// One pointer or wheel event over a viewport. `mouse_pos` is in window
/// logical pixels.
///
/// Drag = orbit, Shift+drag = pan, wheel = zoom; the mapping belongs to the
/// renderer, the slot only forwards the raw input.
#[derive(Default, Clone, Copy, Debug)]
pub struct ViewportInput {
    pub mouse_pos: Option<gpui::Point<Pixels>>,
    /// The left button is held.
    pub mouse_down: bool,
    pub shift: bool,
    /// Wheel travel in pixels (a line counts as 40).
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
/// The shell is generic over this: it hands over a scene, asks for the
/// projection and the painted-frame count, and mounts the renderer's gpui
/// element. Everything domain-specific (meshes, cameras, pipelines) stays
/// behind the associated types.
///
/// Input is not part of this contract: the element routes it to the renderer's
/// [`ViewportContent::input`] itself, because over a native surface (Windows)
/// GPUI's own listeners never see it.
///
/// The methods are associated functions (no `&self`) because a renderer is a
/// marker type: the state lives in the per-session [`Self::Handle`].
pub trait ViewportSlot: 'static {
    /// The renderer's per-session handle (the shell keeps one per session);
    /// typically an `Rc<RefCell<Viewport<_>>>`.
    type Handle: Clone + 'static;
    /// The renderer's scene wire format.
    type Scene: 'static;

    fn new_handle() -> Self::Handle;
    fn update_scene(handle: &Self::Handle, scene: &Self::Scene);
    fn projection(handle: &Self::Handle) -> Option<ViewportProjection>;
    fn painted_frames(handle: &Self::Handle) -> u64;
    /// The renderer's element: [`viewport_element`] of the handle's
    /// [`Viewport`].
    fn element(handle: &Self::Handle) -> gpui::AnyElement;
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{px, size};

    fn bounds(width: f32, height: f32) -> Bounds<Pixels> {
        Bounds {
            origin: gpui::Point {
                x: px(0.0),
                y: px(0.0),
            },
            size: size(px(width), px(height)),
        }
    }

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
        assert_eq!(device_size(&bounds(100.0, 50.0), 1.0), (100, 50));
        assert_eq!(device_size(&bounds(100.0, 50.0), 2.0), (200, 100));
        // A zero-sized bounds still yields a renderable target.
        assert_eq!(device_size(&bounds(0.0, 0.0), 1.0), (1, 1));
        // The clamp keeps a huge HiDPI window from allocating a 16k target.
        assert_eq!(device_size(&bounds(4096.0, 4096.0), 8.0), (4096, 4096));
        // A device with a smaller texture limit clamps further.
        assert_eq!(
            clamped_device_size(&bounds(4096.0, 100.0), 1.0, 2048),
            (2048, 100)
        );
    }

    #[test]
    fn aspect_guards_a_zero_height() {
        assert_eq!(aspect((100, 50)), 2.0);
        assert_eq!(aspect((100, 0)), 100.0);
        assert_eq!(aspect((0, 0)), 1.0);
    }
}
