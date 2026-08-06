//! EGL/GLES rendering for layer-shell wallpaper surfaces.
//!
//! All drawing goes through the wl_egl_window-backed EGL window surface:
//! wallpapers are plain GL textures uploaded from decoded pixels, and the
//! diagonal wipe is a fragment-shader half-plane blend of old+new textures.
//! No wl_shm/dmabuf plumbing — Mesa imports the window surface as a dmabuf
//! internally (zero-copy), and redraws only happen during transitions.

use glow::HasContext;
use khronos_egl::{self as egl, Instance, Static};

pub struct Renderer {
    pub egl: Instance<Static>,
    pub display: egl::Display,
    context: egl::Context,
    surface: egl::Surface,
    pub gl: glow::Context,
    prog_blit: glow::Program,
    prog_wipe: glow::Program,
    u_tex: Option<glow::UniformLocation>,
    u_scale: Option<glow::UniformLocation>,
    u_old: Option<glow::UniformLocation>,
    u_new: Option<glow::UniformLocation>,
    u_old_scale: Option<glow::UniformLocation>,
    u_new_scale: Option<glow::UniformLocation>,
    u_progress: Option<glow::UniformLocation>,
    u_feather: Option<glow::UniformLocation>,
    u_size: Option<glow::UniformLocation>,
}

const VERT_SRC: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "const vec2 pos[3] = vec2[3](vec2(-1.0,-1.0), vec2(3.0,-1.0), vec2(-1.0,3.0));\n",
    "const vec2 uv0[3] = vec2[3](vec2(0.0, 1.0), vec2(2.0, 1.0), vec2(0.0,-1.0));\n",
    "out vec2 vUV;\n",
    "void main() {\n",
    "    vUV = uv0[gl_VertexID];\n",
    "    gl_Position = vec4(pos[gl_VertexID], 0.0, 1.0);\n",
    "}\n"
);

const FRAG_BLIT: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "in vec2 vUV;\n",
    "uniform sampler2D uTex;\n",
    // uScale = size of the texture window sampled across the screen (centered).
    // cover: both components <= 1 (crop overflow). contain: one may be > 1 (letterbox).
    "uniform vec2 uScale;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec2 uv = 0.5 + (clamp(vUV, 0.0, 1.0) - 0.5) * uScale;\n",
    "    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0) {\n",
    "        fragColor = vec4(0.0, 0.0, 0.0, 1.0);\n", // contain letterbox
    "    } else {\n",
    "        fragColor = vec4(texture(uTex, uv).rgb, 1.0);\n",
    "    }\n",
    "}\n"
);

/// Diagonal wipe metric (matches the patched hyprpaper):
///   m = ((1-x)*W + y*H)/(W+H)  — 0 at top-right, 1 at bottom-left.
/// New wallpaper revealed where m <= progress, soft-feathered edge.
/// Each texture uses its own cover/contain UV scale (uOldScale / uNewScale).
const FRAG_WIPE: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "in vec2 vUV;\n",
    "uniform sampler2D uOld;\n",
    "uniform sampler2D uNew;\n",
    "uniform vec2 uOldScale;\n",
    "uniform vec2 uNewScale;\n",
    "uniform float uProgress;\n",
    "uniform float uFeather; // normalized by (W+H)\n",
    "uniform vec2  uSize;\n",
    "out vec4 fragColor;\n",
    "vec4 sampleFit(sampler2D tex, vec2 scale, vec2 screenUv) {\n",
    "    vec2 uv = 0.5 + (screenUv - 0.5) * scale;\n",
    "    if (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0)\n",
    "        return vec4(0.0, 0.0, 0.0, 1.0);\n",
    "    return vec4(texture(tex, uv).rgb, 1.0);\n",
    "}\n",
    "void main() {\n",
    "    vec2 screenUv = clamp(vUV, 0.0, 1.0);\n",
    "    vec4 cOld = sampleFit(uOld, uOldScale, screenUv);\n",
    "    vec4 cNew = sampleFit(uNew, uNewScale, screenUv);\n",
    "    float m = ((1.0 - screenUv.x) * uSize.x + screenUv.y * uSize.y) / max(uSize.x + uSize.y, 1.0);\n",
    "    float f = max(uFeather, 0.0005);\n",
    "    float a = smoothstep(uProgress - f, uProgress + f, m);\n",
    "    fragColor = vec4(mix(cNew, cOld, a).rgb, 1.0);\n",
    "}\n"
);

// EGL_IMG_context_priority
const EGL_CONTEXT_PRIORITY_LEVEL_IMG: egl::Int = 0x3100;
const EGL_CONTEXT_PRIORITY_LOW_IMG: egl::Int = 0x3102;

impl Renderer {
    /// Create shared EGL state (display/context) + compile shader programs.
    pub fn new(wl_display: &wayland_client::Connection) -> Result<Self, String> {
        let egl = Instance::new(Static);

        let raw = wl_display.backend().display_ptr() as *mut std::ffi::c_void;
        let display = unsafe { egl.get_display(raw) }.ok_or("eglGetDisplay failed")?;

        let (major, minor) = egl.initialize(display).map_err(|e| format!("eglInitialize: {e:?}"))?;
        let api_exts = egl
            .query_string(None, egl::EXTENSIONS)
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        log::debug!("EGL {major}.{minor}; client exts: {api_exts}");

        // Low-priority context where supported: a wallpaper should never
        // preempt the UI. (Mesa desktop doesn't advertise IMG_context_priority;
        // the attribute is only included when the extension is actually there.)
        let mut ctx_attrs: Vec<egl::Int> = vec![egl::CONTEXT_CLIENT_VERSION, 3];
        if api_exts.contains("EGL_IMG_context_priority") {
            ctx_attrs.push(EGL_CONTEXT_PRIORITY_LEVEL_IMG);
            ctx_attrs.push(EGL_CONTEXT_PRIORITY_LOW_IMG);
        }
        ctx_attrs.push(egl::NONE);

        let config = pick_config(&egl, display, egl::WINDOW_BIT | egl::PBUFFER_BIT)?;
        let context = egl
            .create_context(display, config, None, &ctx_attrs)
            .map_err(|e| format!("eglCreateContext: {e:?}"))?;

        // 1×1 pbuffer until the first window surface attaches.
        let surface = egl
            .create_pbuffer_surface(display, config, &[egl::WIDTH, 1, egl::HEIGHT, 1, egl::NONE])
            .map_err(|e| format!("pbuffer: {e:?}"))?;
        egl.make_current(display, Some(surface), Some(surface), Some(context))
            .map_err(|e| format!("make_current: {e:?}"))?;

        let gl = unsafe {
            glow::Context::from_loader_function(|s| {
                egl.get_proc_address(s)
                    .map(|f| f as *const std::ffi::c_void)
                    .unwrap_or(std::ptr::null())
            })
        };
        log::debug!(
            "GL: {} | {} | {}",
            unsafe { gl.get_parameter_string(glow::VENDOR) },
            unsafe { gl.get_parameter_string(glow::RENDERER) },
            unsafe { gl.get_parameter_string(glow::VERSION) }
        );

        let prog_blit = link_program(&gl, VERT_SRC, FRAG_BLIT)?;
        let prog_wipe = link_program(&gl, VERT_SRC, FRAG_WIPE)?;

        let u = |prog: glow::Program, name: &str| unsafe { gl.get_uniform_location(prog, name) };
        Ok(Renderer {
            egl,
            display,
            context,
            surface,
            u_tex: u(prog_blit, "uTex"),
            u_scale: u(prog_blit, "uScale"),
            u_old: u(prog_wipe, "uOld"),
            u_new: u(prog_wipe, "uNew"),
            u_old_scale: u(prog_wipe, "uOldScale"),
            u_new_scale: u(prog_wipe, "uNewScale"),
            u_progress: u(prog_wipe, "uProgress"),
            u_feather: u(prog_wipe, "uFeather"),
            u_size: u(prog_wipe, "uSize"),
            gl,
            prog_blit,
            prog_wipe,
        })
    }

    /// Point EGL at an output's wl_egl_window (already sized).
    pub fn attach_window(&mut self, window: &wayland_egl::WlEglSurface) -> Result<(), String> {
        let config = pick_config(&self.egl, self.display, egl::WINDOW_BIT)?;
        let surface = unsafe {
            self.egl
                .create_window_surface(self.display, config, window.ptr() as egl::NativeWindowType, None)
        }
        .map_err(|e| format!("create_window_surface: {e:?}"))?;
        self.egl
            .make_current(self.display, Some(surface), Some(surface), Some(self.context))
            .map_err(|e| format!("make_current(window): {e:?}"))?;
        let old = self.surface;
        self.surface = surface;
        self.egl.destroy_surface(self.display, old).ok();
        Ok(())
    }

    pub fn swap(&self) {
        self.egl.swap_buffers(self.display, self.surface).ok();
    }

    /// Upload RGBA pixels as a GL texture.
    pub fn upload_rgba(&self, pixels: &[u8], w: u32, h: u32) -> glow::Texture {
        let gl = &self.gl;
        let tex = unsafe { gl.create_texture().expect("create texture") };
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                w as i32,
                h as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(pixels)),
            );
            gl.generate_mipmap(glow::TEXTURE_2D);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        tex
    }

    pub fn delete_texture(&self, tex: glow::Texture) {
        unsafe { self.gl.delete_texture(tex) };
    }

    /// Static draw: one textured fullscreen triangle.
    pub fn draw_blit(&self, vw: i32, vh: i32, tex: glow::Texture, img_w: u32, img_h: u32, fit: crate::config::FitMode) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.clear_color(0.0, 0.0, 0.0, 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.use_program(Some(self.prog_blit));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_tex.as_ref(), 0);
            let (sx, sy) = fit_uv_scale(vw, vh, img_w, img_h, fit);
            gl.uniform_2_f32(self.u_scale.as_ref(), sx, sy);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Wipe draw: old→new diagonal half-plane blend at `progress` (0..=1).
    /// `feather_px` is the edge half-width in output pixels.
    /// Both textures use the same `fit` against the output size.
    pub fn draw_wipe(
        &self,
        vw: i32,
        vh: i32,
        old: glow::Texture,
        old_w: u32,
        old_h: u32,
        new: glow::Texture,
        new_w: u32,
        new_h: u32,
        fit: crate::config::FitMode,
        progress: f32,
        feather_px: f32,
    ) {
        let gl = &self.gl;
        let (osx, osy) = fit_uv_scale(vw, vh, old_w, old_h, fit);
        let (nsx, nsy) = fit_uv_scale(vw, vh, new_w, new_h, fit);
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_wipe));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(old));
            gl.uniform_1_i32(self.u_old.as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(new));
            gl.uniform_1_i32(self.u_new.as_ref(), 1);
            gl.uniform_2_f32(self.u_old_scale.as_ref(), osx, osy);
            gl.uniform_2_f32(self.u_new_scale.as_ref(), nsx, nsy);
            gl.uniform_1_f32(self.u_progress.as_ref(), progress);
            let diag = (vw + vh) as f32;
            gl.uniform_1_f32(self.u_feather.as_ref(), feather_px / diag.max(1.0));
            gl.uniform_2_f32(self.u_size.as_ref(), vw as f32, vh as f32);
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }
}

/// UV window size sampled across the full screen (centered).
///
/// Shader: `uv = 0.5 + (screenUv - 0.5) * scale`
/// - **cover**: both axes ≤ 1 → crop the long side of the image (no stretch)
/// - **contain**: one axis ≥ 1 → letterbox (shader paints black outside 0..1)
/// - **fill**: (1,1) → stretch to fill
///
/// Earlier this returned the *inverse* (image/screen), which pushed UVs outside
/// the texture and `CLAMP_TO_EDGE` stretched the top/bottom edges.
fn fit_uv_scale(vw: i32, vh: i32, iw: u32, ih: u32, fit: crate::config::FitMode) -> (f32, f32) {
    let (vw, vh, iw, ih) = (vw as f32, vh as f32, iw as f32, ih as f32);
    if iw <= 0.0 || ih <= 0.0 || vw <= 0.0 || vh <= 0.0 {
        return (1.0, 1.0);
    }
    match fit {
        crate::config::FitMode::Cover => {
            let s = f32::max(vw / iw, vh / ih);
            (vw / (iw * s), vh / (ih * s))
        }
        crate::config::FitMode::Contain => {
            let s = f32::min(vw / iw, vh / ih);
            (vw / (iw * s), vh / (ih * s))
        }
        crate::config::FitMode::Fill => (1.0, 1.0),
    }
}

fn pick_config(egl: &Instance<Static>, display: egl::Display, surface_bits: egl::Int) -> Result<egl::Config, String> {
    let attrs: &[egl::Int] = &[
        egl::SURFACE_TYPE,
        surface_bits,
        egl::RENDERABLE_TYPE,
        egl::OPENGL_ES3_BIT,
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::NONE,
    ];
    let mut configs = Vec::with_capacity(16);
    egl.choose_config(display, attrs, &mut configs).map_err(|e| format!("eglChooseConfigs: {e:?}"))?;
    configs.into_iter().next().ok_or_else(|| "no suitable EGL config".to_string())
}

fn compile_shader(gl: &glow::Context, kind: u32, src: &str) -> Result<glow::Shader, String> {
    let s = unsafe { gl.create_shader(kind) }.map_err(|e| format!("create_shader: {e}"))?;
    unsafe {
        gl.shader_source(s, src);
        gl.compile_shader(s);
    }
    if !unsafe { gl.get_shader_compile_status(s) } {
        let msg = unsafe { gl.get_shader_info_log(s) };
        unsafe { gl.delete_shader(s) };
        return Err(format!("shader compile: {msg}"));
    }
    Ok(s)
}

fn link_program(gl: &glow::Context, vert: &str, frag: &str) -> Result<glow::Program, String> {
    let vs = compile_shader(gl, glow::VERTEX_SHADER, vert)?;
    let fs = compile_shader(gl, glow::FRAGMENT_SHADER, frag)?;
    let prog = unsafe { gl.create_program() }.map_err(|e| format!("create_program: {e}"))?;
    unsafe {
        gl.attach_shader(prog, vs);
        gl.attach_shader(prog, fs);
        gl.link_program(prog);
    }
    unsafe {
        gl.delete_shader(vs);
        gl.delete_shader(fs);
    }
    if !unsafe { gl.get_program_link_status(prog) } {
        let msg = unsafe { gl.get_program_info_log(prog) };
        unsafe { gl.delete_program(prog) };
        return Err(format!("program link: {msg}"));
    }
    Ok(prog)
}
