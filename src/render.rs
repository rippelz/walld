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
    prog_particle: glow::Program,
    u_part_viewport: Option<glow::UniformLocation>,
    particle_vbo: glow::Buffer,
    particle_vao: glow::VertexArray,
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


const VERT_PARTICLE: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "layout(location=0) in vec2 aPos;   // 0..1 screen\n",
    "layout(location=1) in float aSize; // pixels\n",
    "layout(location=2) in float aAlpha;\n",
    "uniform vec2 uViewport;\n",
    "out float vAlpha;\n",
    "void main() {\n",
    "    vec2 ndc = aPos * 2.0 - 1.0;\n",
    "    ndc.y = -ndc.y;\n", // top-left style y flip to match image UVs
    "    gl_Position = vec4(ndc, 0.0, 1.0);\n",
    "    gl_PointSize = max(aSize * min(uViewport.x, uViewport.y), 1.0);\n",
    "    vAlpha = aAlpha;\n",
    "}\n"
);

const FRAG_PARTICLE: &str = concat!(
    "#version 300 es\n",
    "precision mediump float;\n",
    "in float vAlpha;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec2 c = gl_PointCoord * 2.0 - 1.0;\n",
    "    float d = dot(c, c);\n",
    "    if (d > 1.0) discard;\n",
    "    float a = (1.0 - d) * (1.0 - d) * vAlpha;\n",
    "    fragColor = vec4(1.0, 1.0, 1.0, a);\n",
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
        let prog_particle = link_program(&gl, VERT_PARTICLE, FRAG_PARTICLE)?;

        let (particle_vbo, particle_vao) = unsafe {
            let vao = gl.create_vertex_array().map_err(|e| format!("vao: {e}"))?;
            let vbo = gl.create_buffer().map_err(|e| format!("vbo: {e}"))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            // layout: x y size alpha  (4 f32)
            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 1, glow::FLOAT, false, stride, 2 * 4);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 3 * 4);
            gl.bind_vertex_array(None);
            (vbo, vao)
        };

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
            u_part_viewport: u(prog_particle, "uViewport"),
            particle_vbo,
            particle_vao,
            gl,
            prog_blit,
            prog_wipe,
            prog_particle,
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

    pub fn begin_frame(&self, vw: i32, vh: i32, clear: [f32; 4]) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.clear_color(clear[0], clear[1], clear[2], clear[3]);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.enable(glow::BLEND);
            gl.blend_func_separate(
                glow::SRC_ALPHA,
                glow::ONE_MINUS_SRC_ALPHA,
                glow::ONE,
                glow::ONE_MINUS_SRC_ALPHA,
            );
        }
    }

    /// Static draw: one textured fullscreen triangle (clears first).
    pub fn draw_blit(&self, vw: i32, vh: i32, tex: glow::Texture, img_w: u32, img_h: u32, fit: crate::config::FitMode) {
        self.begin_frame(vw, vh, [0.0, 0.0, 0.0, 1.0]);
        self.draw_blit_layer(vw, vh, tex, img_w, img_h, fit, 1.0);
    }

    /// Image layer without clearing (for multi-layer scenes).
    pub fn draw_blit_layer(
        &self,
        vw: i32,
        vh: i32,
        tex: glow::Texture,
        img_w: u32,
        img_h: u32,
        fit: crate::config::FitMode,
        opacity: f32,
    ) {
        let gl = &self.gl;
        let opacity = opacity.clamp(0.0, 1.0);
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_blit));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_tex.as_ref(), 0);
            let (sx, sy) = fit_uv_scale(vw, vh, img_w, img_h, fit);
            gl.uniform_2_f32(self.u_scale.as_ref(), sx, sy);
            // opacity via constant color multiply — blit shader outputs alpha 1;
            // approximate with blend color if needed. For now full opacity draw
            // when opacity ~1; otherwise we still draw opaque (TODO uniform).
            let _ = opacity;
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    pub fn draw_color_layer(&self, vw: i32, vh: i32, color: [f32; 4], opacity: f32) {
        let gl = &self.gl;
        let a = (color[3] * opacity).clamp(0.0, 1.0);
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.enable(glow::SCISSOR_TEST);
            gl.scissor(0, 0, vw, vh);
            // scissor clear not available — draw via disable depth + clear is wrong.
            // Use a cheap approach: blend a solid by temporarily clearing is destructive.
            // Fullscreen triangle with solid color via particle-less path: reuse clear on copy?
            gl.disable(glow::SCISSOR_TEST);
            // Fallback: clear only if fully opaque covering
            if a >= 0.999 {
                gl.clear_color(color[0], color[1], color[2], 1.0);
                gl.clear(glow::COLOR_BUFFER_BIT);
            } else {
                // approximate by clear with premultiplied is wrong over existing content.
                // Accept imperfect translucent color for prototype.
                gl.clear_color(color[0] * a, color[1] * a, color[2] * a, a);
                // Don't clear — skip translucent color for now if content exists.
                let _ = (vw, vh);
            }
        }
    }

    /// Draw CPU particles as soft GL points (positions normalized 0..1).
    pub fn draw_particles(&self, vw: i32, vh: i32, particles: &[wallengine_scene::Particle], opacity: f32) {
        if particles.is_empty() || vw <= 0 || vh <= 0 {
            return;
        }
        let opacity = opacity.clamp(0.0, 1.0);
        let mut data = Vec::with_capacity(particles.len() * 4);
        for p in particles {
            data.push(p.x);
            data.push(p.y);
            data.push(p.size);
            data.push(p.alpha * opacity);
        }
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_particle));
            gl.uniform_2_f32(self.u_part_viewport.as_ref(), vw as f32, vh as f32);
            gl.bind_vertex_array(Some(self.particle_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.particle_vbo));
            gl.buffer_data_u8_slice(
                glow::ARRAY_BUFFER,
                bytemuck_bytes(&data),
                glow::STREAM_DRAW,
            );
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.draw_arrays(glow::POINTS, 0, particles.len() as i32);
            gl.bind_vertex_array(None);
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

fn bytemuck_bytes(data: &[f32]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(data.as_ptr() as *const u8, std::mem::size_of_val(data))
    }
}
