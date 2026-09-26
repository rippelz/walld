//! Wayland-side state: outputs, layer surfaces, wipe animation state.

use std::path::PathBuf;
use std::time::Instant;

use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::ZwlrLayerSurfaceV1;

use crate::config::FitMode;

pub const NAMESPACE: &str = "walld";

#[derive(Clone, Debug, Default)]
pub struct OutputInfo {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub scale: f64,
}

impl OutputInfo {
    pub fn ready(&self) -> bool {
        !self.name.is_empty() && self.width > 0 && self.height > 0
    }
}

/// A decoded+uploaded wallpaper / WE layer texture.
#[derive(Clone, Copy)]
pub struct Texture {
    pub tex: glow::Texture,
    pub w: u32,
    pub h: u32,
    /// Sample only this UV rect (content inside NPOT-padded buffer). Default (1,1).
    pub uv_scale: (f32, f32),
}

/// A wallpaper that should be shown, with the transition used to reveal it.
pub struct PendingWall {
    pub path: PathBuf,
    pub transition: crate::config::Transition,
}

pub struct SurfaceState {
    pub wl_surface: WlSurface,
    pub layer: ZwlrLayerSurfaceV1,
    /// EGL window for the wl_surface (destroyed before the surface itself).
    pub egl_window: Option<wayland_egl::WlEglSurface>,
    pub width: u32,
    pub height: u32,
    /// True once a frame has been committed at least once (`walld ctl ready`).
    pub committed: bool,
}

pub struct WipeState {
    pub old: Texture,
    pub new: Texture,
    pub start: Instant,
    pub dur_ms: u32,
    pub feather_px: f32,
}

/// In-flight WE→WE wipe: `tex` is a still of the outgoing scene captured into an
/// FBO texture (window readback would freeze WE video on AMD/Mesa); the live
/// incoming scene renders beneath it every frame in `draw_we_on`.
pub struct WipeWe {
    pub tex: glow::Texture,
    pub start: Instant,
    pub dur_ms: u32,
    pub feather_px: f32,
    /// Timestamp of the last overlay draw — used to pause the clock while the
    /// incoming content has nothing drawable yet (e.g. video pre-first-frame),
    /// so the wipe resumes from zero instead of jumping ahead over black.
    pub last_draw: Instant,
}

pub struct Output {
    pub wl_output: WlOutput,
    /// Registry global name of this output (matches GlobalRemove).
    pub global_name: u32,
    pub info: OutputInfo,
    pub surface: Option<SurfaceState>,
    /// Wallpaper currently on screen (or the wipe's target while animating).
    pub current: Option<Texture>,
    pub current_path: Option<PathBuf>,
    pub fit: FitMode,
    /// Path queued for decode, if any.
    pub pending_path: Option<PathBuf>,
    /// Transition queued for the pending path.
    pub pending_transition: crate::config::Transition,
    pub wipe: Option<WipeState>,
    /// Wallpaper Engine crossfade (old still → live new scene).
    pub wipe_we: Option<WipeWe>,
    /// Set while a just-armed `wipe_we` has not had its first overlay draw on
    /// this output. The arm pass paints the outgoing still over the incoming
    /// content once, so the video's very first frame (possibly black before
    /// the decoder warms up) must not be blitted until the overlay has drawn
    /// atop real content — see `draw_we_on`.
    pub wipe_pending_first: bool,
}

pub fn ease_out_cubic(t: f32) -> f32 {
    let u = 1.0 - t;
    1.0 - u * u * u
}
