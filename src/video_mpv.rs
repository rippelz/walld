//! LWE-style video playback via **libmpv** + OpenGL render API.
//!
//! Matches linux-wallpaperengine's `VideoPlayback/MPV/GLPlayer`:
//! - `vo=libmpv`, `hwdec=auto`, `loop=inf`, `profile=fast`
//! - Each frame: `mpv_render_context_render` into an FBO we own
//! - No full-frame CPU RGBA upload path (that's the ffmpeg fallback)

use crate::video::{VideoControl, VideoInfo};
use glow::HasContext;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex};

use libmpv_sys as mpv;

/// EGL proc address (same approach as LWE's get_proc_address).
#[link(name = "EGL")]
extern "C" {
    fn eglGetProcAddress(procname: *const c_char) -> *mut c_void;
}

unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    eglGetProcAddress(name)
}

pub struct MpvGlDecoder {
    pub path: PathBuf,
    pub info: VideoInfo,
    pub out_w: u32,
    pub out_h: u32,
    control: Arc<Mutex<VideoControl>>,
    handle: *mut mpv::mpv_handle,
    render: *mut mpv::mpv_render_context,
    fbo: Option<glow::Framebuffer>,
    tex: Option<glow::Texture>,
    tex_w: u32,
    tex_h: u32,
    gl_ready: bool,
    /// True once we have rendered at least one frame with a real video size.
    /// Until then callers must keep any poster/fallback texture.
    has_frame: bool,
}

// mpv handles are not Send by default; we only touch them from the GL/render thread.
unsafe impl Send for MpvGlDecoder {}

impl MpvGlDecoder {
    pub fn start(path: &Path, max_edge: u32) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("video not found: {}", path.display()));
        }
        let info = crate::video::probe_video(path).unwrap_or(VideoInfo {
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration_secs: 0.0,
        });
        let max_edge = max_edge.clamp(640, 7680);
        let (out_w, out_h) = fit_max_edge(info.width, info.height, max_edge);

        let handle = unsafe { mpv::mpv_create() };
        if handle.is_null() {
            return Err("mpv_create failed".into());
        }

        // LWE options (GLPlayer::init) + GLES-safe embed defaults.
        set_opt(handle, "terminal", "no")?;
        set_opt(handle, "msg-level", "all=error")?;
        set_opt(handle, "input-cursor", "no")?;
        set_opt(handle, "cursor-autohide", "no")?;
        set_opt(handle, "config", "no")?;
        set_opt(handle, "fbo-format", "rgba8")?;
        set_opt(handle, "vo", "libmpv")?;
        set_opt(handle, "profile", "fast")?;
        // walld is GLES 3 — without this mpv often renders pure black into FBOs.
        set_opt(handle, "opengl-es", "yes")?;
        // Keep real-time pacing (LWE default untimed=no).
        set_opt(handle, "untimed", "no")?;
        set_opt(handle, "interpolation", "no")?;
        set_opt(handle, "audio", "no")?;

        let rc = unsafe { mpv::mpv_initialize(handle) };
        if rc < 0 {
            unsafe { mpv::mpv_terminate_destroy(handle) };
            return Err(format!("mpv_initialize: {rc}"));
        }

        // Default `no` (software into FBO) — hwdec paths (CUDA/VAAPI interop) were
        // leaving pure-black FBOs and blanking whole WE scenes. Opt in via env:
        // WALLD_MPV_HWDEC=vaapi,auto-copy,no  or  auto
        let hwdec = std::env::var("WALLD_MPV_HWDEC").unwrap_or_else(|_| "no".into());
        set_prop(handle, "hwdec", &hwdec)?;
        set_prop(handle, "loop", "inf")?;
        set_prop(handle, "mute", "yes")?;
        set_prop(handle, "keep-open", "yes")?;
        // Display-sync needs a real display; for FBO embed use audio (silent) clock.
        set_prop(handle, "video-sync", "audio")?;

        // Limit decode size roughly to desktop (mpv scale).
        if out_w < info.width || out_h < info.height {
            let vf = format!("scale={out_w}:{out_h}:force_original_aspect_ratio=decrease");
            set_prop(handle, "vf", &vf)?;
        }

        let loadfile = CString::new("loadfile").unwrap();
        let path_c = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| "path has interior NUL".to_string())?;
        let mut cmd = [loadfile.as_ptr(), path_c.as_ptr(), ptr::null()];
        let rc = unsafe { mpv::mpv_command(handle, cmd.as_mut_ptr()) };
        if rc < 0 {
            unsafe { mpv::mpv_terminate_destroy(handle) };
            return Err(format!("mpv loadfile: {rc}"));
        }

        let control = Arc::new(Mutex::new(VideoControl {
            playing: true,
            looping: true,
            time: 0.0,
            duration: info.duration_secs.max(0.0),
            rate: 1.0,
            pace_epoch: 0,
        }));

        log::info!(
            "video[mpv] {} {}×{} @ {:.3}fps → {}×{} (hwdec={hwdec})",
            path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
            info.width,
            info.height,
            info.fps,
            out_w,
            out_h,
        );

        Ok(Self {
            path: path.to_path_buf(),
            info,
            out_w,
            out_h,
            control,
            handle,
            render: ptr::null_mut(),
            fbo: None,
            tex: None,
            tex_w: 0,
            tex_h: 0,
            gl_ready: false,
            has_frame: false,
        })
    }

    pub fn control(&self) -> Arc<Mutex<VideoControl>> {
        self.control.clone()
    }

    pub fn control_snapshot(&self) -> VideoControl {
        // Refresh time/duration from mpv when possible.
        if let Ok(mut g) = self.control.lock() {
            if let Some(t) = get_prop_f64(self.handle, "time-pos") {
                g.time = t as f32;
            }
            if let Some(d) = get_prop_f64(self.handle, "duration") {
                if d.is_finite() && d > 0.0 {
                    g.duration = d as f32;
                }
            }
            g.clone()
        } else {
            VideoControl::default()
        }
    }

    pub fn set_user_playing(&self, playing: bool) {
        if let Ok(mut g) = self.control.lock() {
            if g.playing != playing {
                g.playing = playing;
                g.pace_epoch = g.pace_epoch.wrapping_add(1);
            }
        }
        let _ = set_prop(self.handle, "pause", if playing { "no" } else { "yes" });
    }

    pub fn set_rate(&self, rate: f32) {
        let rate = rate.clamp(0.05, 4.0);
        if let Ok(mut g) = self.control.lock() {
            if (g.rate - rate).abs() > 0.0001 {
                g.rate = rate;
                g.pace_epoch = g.pace_epoch.wrapping_add(1);
            }
        }
        // mpv speed property
        let s = format!("{rate:.4}");
        let _ = set_prop(self.handle, "speed", &s);
    }

    pub fn apply_script_control(&self, playing: bool, looping: bool, seek_to: Option<f32>) {
        let honor = std::env::var("WALLD_VIDEO_SCRIPT_CONTROL")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !honor {
            return;
        }
        self.set_user_playing(playing);
        if let Ok(mut g) = self.control.lock() {
            g.looping = looping;
        }
        let _ = set_prop(self.handle, "loop", if looping { "inf" } else { "no" });
        // libmpv can mid-stream seek (LWE advantage over ffmpeg pipe).
        if let Some(t) = seek_to {
            let s = format!("{t:.4}");
            let _ = set_prop(self.handle, "time-pos", &s);
        }
    }

    /// Create render context + FBO once the process GL context is current.
    pub fn ensure_gl(&mut self, gl: &glow::Context) -> Result<(), String> {
        if self.gl_ready {
            return Ok(());
        }

        let mut gl_init = mpv::mpv_opengl_init_params {
            get_proc_address: Some(get_proc_address),
            get_proc_address_ctx: ptr::null_mut(),
            extra_exts: ptr::null(),
        };

        // API type data is the char* itself (LWE / mpv docs).
        let mut params = [
            mpv::mpv_render_param {
                type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_API_TYPE,
                data: mpv::MPV_RENDER_API_TYPE_OPENGL.as_ptr() as *mut c_void,
            },
            mpv::mpv_render_param {
                type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_INIT_PARAMS,
                data: &mut gl_init as *mut _ as *mut c_void,
            },
            mpv::mpv_render_param {
                type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];

        let mut render: *mut mpv::mpv_render_context = ptr::null_mut();
        let rc = unsafe {
            mpv::mpv_render_context_create(&mut render, self.handle, params.as_mut_ptr())
        };
        if rc < 0 || render.is_null() {
            return Err(format!("mpv_render_context_create: {rc}"));
        }
        self.render = render;

        self.ensure_target(gl, self.out_w.max(2), self.out_h.max(2))?;
        self.gl_ready = true;
        log::info!("video[mpv] GL render context ready {}×{}", self.out_w, self.out_h);
        Ok(())
    }

    fn ensure_target(&mut self, gl: &glow::Context, w: u32, h: u32) -> Result<(), String> {
        let w = w.max(2);
        let h = h.max(2);
        if self.tex.is_some() && self.tex_w == w && self.tex_h == h {
            return Ok(());
        }
        unsafe {
            if let Some(t) = self.tex.take() {
                gl.delete_texture(t);
            }
            if let Some(f) = self.fbo.take() {
                gl.delete_framebuffer(f);
            }
            let tex = gl.create_texture().map_err(|e| e.to_string())?;
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);

            let fbo = gl.create_framebuffer().map_err(|e| e.to_string())?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            let status = gl.check_framebuffer_status(glow::FRAMEBUFFER);
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            if status != glow::FRAMEBUFFER_COMPLETE {
                return Err(format!("mpv FBO incomplete: {status:#x}"));
            }
            self.tex = Some(tex);
            self.fbo = Some(fbo);
            self.tex_w = w;
            self.tex_h = h;
        }
        Ok(())
    }

    /// Render current frame into our FBO. Returns texture handle for sampling.
    ///
    /// Returns `Err("not ready")` until the video has reconfigured and at least
    /// one frame has been drawn — callers must keep any poster texture until then
    /// (replacing it with the empty FBO blanks whole WE scenes).
    pub fn render(&mut self, gl: &glow::Context) -> Result<(glow::Texture, u32, u32), String> {
        self.ensure_gl(gl)?;

        // Prefer live dwidth/dheight once video reconfigured.
        let mut w = self.out_w;
        let mut h = self.out_h;
        let mut sized = false;
        if let (Some(dw), Some(dh)) = (
            get_prop_i64(self.handle, "dwidth"),
            get_prop_i64(self.handle, "dheight"),
        ) {
            if dw > 0 && dh > 0 {
                w = dw as u32;
                h = dh as u32;
                sized = true;
            }
        }
        self.ensure_target(gl, w, h)?;

        // Drain events (VIDEO_RECONFIG etc.)
        loop {
            let ev = unsafe { mpv::mpv_wait_event(self.handle, 0.0) };
            if ev.is_null() {
                break;
            }
            let id = unsafe { (*ev).event_id };
            if id == mpv::mpv_event_id_MPV_EVENT_NONE {
                break;
            }
        }

        // Drive decode; UPDATE_FRAME means a new image is ready to present.
        let update = unsafe { mpv::mpv_render_context_update(self.render) };
        let frame_flag = (update & mpv::mpv_render_update_flag_MPV_RENDER_UPDATE_FRAME as u64) != 0;

        let fbo_id = self.fbo_raw(gl)?;
        // Explicit RGBA8 — `0` (default) is unreliable on some GLES drivers.
        let mut fbo = mpv::mpv_opengl_fbo {
            fbo: fbo_id as c_int,
            w: self.tex_w as c_int,
            h: self.tex_h as c_int,
            internal_format: glow::RGBA8 as c_int,
        };
        let mut flip_y: c_int = 1; // LWE flips; our textures are top-left sample
        let mut params = [
            mpv::mpv_render_param {
                type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_OPENGL_FBO,
                data: &mut fbo as *mut _ as *mut c_void,
            },
            mpv::mpv_render_param {
                type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_FLIP_Y,
                data: &mut flip_y as *mut _ as *mut c_void,
            },
            mpv::mpv_render_param {
                type_: mpv::mpv_render_param_type_MPV_RENDER_PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let rc = unsafe { mpv::mpv_render_context_render(self.render, params.as_mut_ptr()) };
        // Leave default FB bound; draw paths rebind their home target next.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        if rc < 0 {
            return Err(format!("mpv_render_context_render: {rc}"));
        }

        // Ready once mpv says a frame is available, or video has reconfigured
        // and time is advancing. After the first good frame we keep serving the
        // FBO even when UPDATE_FRAME is quiet (paused / same frame).
        let tpos = get_prop_f64(self.handle, "time-pos").unwrap_or(0.0);
        if frame_flag || self.has_frame || (sized && tpos > 0.001) {
            self.has_frame = true;
        }

        let _ = self.control_snapshot();

        if !self.has_frame {
            return Err("mpv frame not ready".into());
        }
        let tex = self.tex.ok_or("mpv texture missing")?;
        Ok((tex, self.tex_w, self.tex_h))
    }

    fn fbo_raw(&self, gl: &glow::Context) -> Result<u32, String> {
        let fbo = self.fbo.ok_or("mpv fbo missing")?;
        // glow::Framebuffer is typically a newtype over NativeFramebuffer(NonZeroU32)
        // Extract raw GL name via bind+get — avoid depending on glow internals.
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            let mut name = 0i32;
            gl.get_parameter_i32_slice(glow::FRAMEBUFFER_BINDING, std::slice::from_mut(&mut name));
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            if name <= 0 {
                return Err("invalid FBO name".into());
            }
            Ok(name as u32)
        }
    }

    /// Steal the texture for drawing without transferring ownership of GL objects
    /// that Drop must free — caller must **not** delete the texture.
    pub fn texture(&self) -> Option<(glow::Texture, u32, u32)> {
        self.tex.map(|t| (t, self.tex_w, self.tex_h))
    }
}

impl Drop for MpvGlDecoder {
    fn drop(&mut self) {
        // GL objects need a current context; best-effort. Main teardown makes
        // current before dropping content.
        if !self.render.is_null() {
            unsafe { mpv::mpv_render_context_free(self.render) };
            self.render = ptr::null_mut();
        }
        if !self.handle.is_null() {
            unsafe { mpv::mpv_terminate_destroy(self.handle) };
            self.handle = ptr::null_mut();
        }
        // Textures/FBOs leak if GL is gone; Renderer drop usually still has EGL.
        // Caller should call destroy_gl before drop when possible.
    }
}

impl MpvGlDecoder {
    pub fn destroy_gl(&mut self, gl: &glow::Context) {
        unsafe {
            if let Some(t) = self.tex.take() {
                gl.delete_texture(t);
            }
            if let Some(f) = self.fbo.take() {
                gl.delete_framebuffer(f);
            }
        }
        if !self.render.is_null() {
            unsafe { mpv::mpv_render_context_free(self.render) };
            self.render = ptr::null_mut();
        }
        self.gl_ready = false;
    }
}

fn set_opt(handle: *mut mpv::mpv_handle, key: &str, val: &str) -> Result<(), String> {
    let k = CString::new(key).unwrap();
    let v = CString::new(val).unwrap();
    let rc = unsafe { mpv::mpv_set_option_string(handle, k.as_ptr(), v.as_ptr()) };
    if rc < 0 {
        Err(format!("mpv option {key}={val}: {rc}"))
    } else {
        Ok(())
    }
}

fn set_prop(handle: *mut mpv::mpv_handle, key: &str, val: &str) -> Result<(), String> {
    let k = CString::new(key).unwrap();
    let v = CString::new(val).unwrap();
    let rc = unsafe { mpv::mpv_set_property_string(handle, k.as_ptr(), v.as_ptr()) };
    if rc < 0 {
        Err(format!("mpv prop {key}={val}: {rc}"))
    } else {
        Ok(())
    }
}

fn get_prop_f64(handle: *mut mpv::mpv_handle, key: &str) -> Option<f64> {
    let k = CString::new(key).ok()?;
    let mut v: f64 = 0.0;
    let rc = unsafe {
        mpv::mpv_get_property(
            handle,
            k.as_ptr(),
            mpv::mpv_format_MPV_FORMAT_DOUBLE,
            &mut v as *mut _ as *mut c_void,
        )
    };
    if rc < 0 {
        None
    } else {
        Some(v)
    }
}

fn get_prop_i64(handle: *mut mpv::mpv_handle, key: &str) -> Option<i64> {
    let k = CString::new(key).ok()?;
    let mut v: i64 = 0;
    let rc = unsafe {
        mpv::mpv_get_property(
            handle,
            k.as_ptr(),
            mpv::mpv_format_MPV_FORMAT_INT64,
            &mut v as *mut _ as *mut c_void,
        )
    };
    if rc < 0 {
        None
    } else {
        Some(v)
    }
}

fn fit_max_edge(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
    let max_edge = max_edge.max(64);
    let long = w.max(h).max(1);
    if long <= max_edge {
        return (w.max(2), h.max(2));
    }
    let s = max_edge as f32 / long as f32;
    (
        ((w as f32 * s).round() as u32).max(2) & !1,
        ((h as f32 * s).round() as u32).max(2) & !1,
    )
}

/// Prefer mpv only when explicitly requested. Default is ffmpeg until the GLES
/// render path is solid on all GPUs (mpv black-FBO was blanking whole scenes).
/// Opt in: `WALLD_VIDEO=mpv`.
pub fn prefer_mpv() -> bool {
    match std::env::var("WALLD_VIDEO") {
        Ok(v) => {
            let v = v.to_ascii_lowercase();
            matches!(v.as_str(), "mpv" | "libmpv" | "1" | "on" | "true")
        }
        Err(_) => false,
    }
}

// silence unused import if CStr not used in all builds
#[allow(dead_code)]
fn _cstr_debug(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
}
