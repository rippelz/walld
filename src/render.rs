//! EGL/GLES rendering for layer-shell wallpaper surfaces.
//!
//! All drawing goes through the wl_egl_window-backed EGL window surface:
//! wallpapers are plain GL textures uploaded from decoded pixels, and the
//! diagonal wipe is a fragment-shader half-plane blend of old+new textures.
//! No wl_shm/dmabuf plumbing — Mesa imports the window surface as a dmabuf
//! internally (zero-copy), and redraws only happen during transitions.

use std::collections::HashMap;

use glow::HasContext;
use khronos_egl::{self as egl, Instance, Static};

pub struct Renderer {
    pub egl: Instance<Static>,
    pub display: egl::Display,
    context: egl::Context,
    surface: egl::Surface,
    /// Cached EGL window surfaces keyed by `wl_egl_window` native pointer.
    /// Re-creating a surface every frame (old attach_window) freezes WE video
    /// on AMD/Mesa — pure black after the first present.
    window_surfaces: HashMap<usize, egl::Surface>,
    /// Native window currently bound as `surface` (if any).
    current_window: Option<usize>,
    pub gl: glow::Context,
    prog_blit: glow::Program,
    prog_wipe: glow::Program,
    prog_we_overlay: glow::Program,
    u_weo_old: Option<glow::UniformLocation>,
    u_weo_progress: Option<glow::UniformLocation>,
    u_weo_feather: Option<glow::UniformLocation>,
    u_weo_size: Option<glow::UniformLocation>,
    u_tex: Option<glow::UniformLocation>,
    u_scale: Option<glow::UniformLocation>,
    u_offset: Option<glow::UniformLocation>,
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
    prog_psprite: glow::Program,
    u_psprite_tex: Option<glow::UniformLocation>,
    u_psprite_overbright: Option<glow::UniformLocation>,
    psprite_vbo: glow::Buffer,
    psprite_vao: glow::VertexArray,
    /// Ortho textured quad (WE image layers).
    prog_ortho: glow::Program,
    u_ortho_mvp: Option<glow::UniformLocation>,
    u_ortho_tex: Option<glow::UniformLocation>,
    u_ortho_opacity: Option<glow::UniformLocation>,
    u_ortho_content_uv: Option<glow::UniformLocation>,
    u_ortho_uv_offset: Option<glow::UniformLocation>,
    u_ortho_key_color: Option<glow::UniformLocation>,
    u_ortho_key_fuzz: Option<glow::UniformLocation>,
    u_ortho_key_tol: Option<glow::UniformLocation>,
    u_ortho_blend_mode: Option<glow::UniformLocation>,
    /// Waterflow effect (WE waterflow).
    prog_waterflow: glow::Program,
    u_wf_mvp: Option<glow::UniformLocation>,
    u_wf_time: Option<glow::UniformLocation>,
    u_wf_speed: Option<glow::UniformLocation>,
    u_wf_feather: Option<glow::UniformLocation>,
    u_wf_amp: Option<glow::UniformLocation>,
    u_wf_phase_scale: Option<glow::UniformLocation>,
    u_wf_tex0: Option<glow::UniformLocation>,
    u_wf_tex1: Option<glow::UniformLocation>,
    u_wf_tex2: Option<glow::UniformLocation>,
    u_wf_tex1_res: Option<glow::UniformLocation>,
    u_wf_frame: Option<glow::UniformLocation>,
    /// Opacity mask (WE effects/opacity)
    prog_opacity: glow::Program,
    u_op_mvp: Option<glow::UniformLocation>,
    u_op_tex0: Option<glow::UniformLocation>,
    u_op_tex1: Option<glow::UniformLocation>,
    u_op_alpha: Option<glow::UniformLocation>,
    u_op_albedo_uv: Option<glow::UniformLocation>,
    u_op_albedo_offset: Option<glow::UniformLocation>,
    u_op_mask_uv: Option<glow::UniformLocation>,
    /// Shared unit-quad VBO: pos.xy uv.xy (also used for streaming puppet verts).
    quad_vbo: glow::Buffer,
    quad_vao: glow::VertexArray,
    /// Fullscreen clip-space quad for WE effect passes. Many workshop/stock
    /// effect verts do `gl_Position = vec4(a_Position, 1.0)` with **no** MVP
    /// (godrays downsample/cast/gaussian, localcontrast, …). Those expect
    /// a_Position already in NDC [-1,1]. The layer unit-quad is ±0.5, so
    /// feeding it here left only a center patch lit and the FBO clear-black
    /// — the classic "black screen + particles" failure.
    effect_quad_vbo: glow::Buffer,
    effect_quad_vao: glow::VertexArray,
    /// util/composelayer: sample the current backbuffer into a layer-sized
    /// target at the layer's projected screen rect (pixelate/vhs censor path).
    prog_compose: glow::Program,
    u_compose_fb: Option<glow::UniformLocation>,
    u_compose_origin: Option<glow::UniformLocation>,
    u_compose_size: Option<glow::UniformLocation>,
    u_compose_angle: Option<glow::UniformLocation>,
    u_compose_ortho: Option<glow::UniformLocation>,
    u_compose_viewport: Option<glow::UniformLocation>,
    /// Home draw target for the current frame (scene RT). Effect passes and
    /// compose sampling restore here so layers after them still hit the RT.
    draw_home: std::cell::Cell<Option<(glow::Framebuffer, u32, u32)>>,
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
    "uniform vec2 uOffset;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec2 base = clamp(vUV, 0.0, 1.0);\n",
    "    vec2 uv = 0.5 + (base - 0.5) * uScale + uOffset;\n",
    "    bool moving = abs(uOffset.x) + abs(uOffset.y) > 0.00001;\n",
    "    if (moving) { uv = fract(uv); }\n",
    "    if (!moving && (uv.x < 0.0 || uv.x > 1.0 || uv.y < 0.0 || uv.y > 1.0)) {\n",
    "        fragColor = vec4(0.0, 0.0, 0.0, 1.0);\n",
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

/// Overlay variant of the wipe for WE switches: the incoming scene is already
/// live on the framebuffer (rendered this frame), so this only paints the
/// outgoing still behind a diagonal feathered alpha that retreats with
/// `uProgress` — same metric as FRAG_WIPE. `uOld` is an FBO-captured still
/// (GL bottom-left origin), hence the flipped-v sample.
const FRAG_WE_OVERLAY: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "in vec2 vUV;\n",
    "uniform sampler2D uOld;\n",
    "uniform float uProgress;\n",
    "uniform float uFeather; // normalized by (W+H)\n",
    "uniform vec2  uSize;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec2 screenUv = clamp(vUV, 0.0, 1.0);\n",
    "    vec4 cOld = texture(uOld, vec2(screenUv.x, 1.0 - screenUv.y));\n",
    "    float m = ((1.0 - screenUv.x) * uSize.x + screenUv.y * uSize.y) / max(uSize.x + uSize.y, 1.0);\n",
    "    float f = max(uFeather, 0.0005);\n",
    "    float a = smoothstep(uProgress - f, uProgress + f, m);\n",
    "    fragColor = vec4(cOld.rgb, a);\n",
    "}\n"
);

const VERT_PARTICLE: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "layout(location=0) in vec2 aPos;    // 0..1 screen UV\n",
    "layout(location=1) in float aSize;  // normalized size (×min(viewport))\n",
    "layout(location=2) in float aAlpha;\n",
    "layout(location=3) in vec3 aColor;\n",
    "uniform vec2 uViewport;\n",
    "out float vAlpha;\n",
    "out vec3 vColor;\n",
    "void main() {\n",
    "    vec2 ndc = aPos * 2.0 - 1.0;\n",
    "    ndc.y = -ndc.y;\n", // top-left style y flip to match image UVs
    "    gl_Position = vec4(ndc, 0.0, 1.0);\n",
    "    gl_PointSize = max(aSize * min(uViewport.x, uViewport.y), 1.0);\n",
    "    vAlpha = aAlpha;\n",
    "    vColor = aColor;\n",
    "}\n"
);

/// Textured particle sprites: CPU-expanded quads in normalized viewport space.
/// layout: pos.xy (0..1), uv.xy, alpha, color.rgb  (8 f32)
const VERT_PSPRITE: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "layout(location=0) in vec2 aPos;\n",
    "layout(location=1) in vec2 aUV;\n",
    "layout(location=2) in float aAlpha;\n",
    "layout(location=3) in vec3 aColor;\n",
    "out vec2 vUV;\n",
    "out float vAlpha;\n",
    "out vec3 vColor;\n",
    "void main() {\n",
    "    vec2 ndc = aPos * 2.0 - 1.0;\n",
    "    ndc.y = -ndc.y;\n",
    "    gl_Position = vec4(ndc, 0.0, 1.0);\n",
    "    vUV = aUV;\n",
    "    vAlpha = aAlpha;\n",
    "    vColor = aColor;\n",
    "}\n"
);

const FRAG_PSPRITE: &str = concat!(
    "#version 300 es\n",
    "precision mediump float;\n",
    "in vec2 vUV;\n",
    "in float vAlpha;\n",
    "in vec3 vColor;\n",
    "uniform sampler2D uTex;\n",
    "uniform float uOverbright;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec4 t = texture(uTex, vUV);\n",
    "    fragColor = vec4(t.rgb * vColor * uOverbright, t.a * vAlpha);\n",
    "}\n"
);

const FRAG_PARTICLE: &str = concat!(
    "#version 300 es\n",
    "precision mediump float;\n",
    "in float vAlpha;\n",
    "in vec3 vColor;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec2 c = gl_PointCoord * 2.0 - 1.0;\n",
    "    float d = dot(c, c);\n",
    "    if (d > 1.0) discard;\n",
    "    float a = (1.0 - d) * (1.0 - d) * vAlpha;\n",
    "    fragColor = vec4(vColor, a);\n",
    "}\n"
);

/// Unit quad: pos xy in local [-0.5,0.5], uv 0..1 (v flipped for image top-left).
const VERT_ORTHO: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "layout(location=0) in vec2 aPos;\n",
    "layout(location=1) in vec2 aUV;\n",
    "uniform mat4 uMVP;\n",
    "out vec2 vUV;\n",
    "void main() {\n",
    "    gl_Position = uMVP * vec4(aPos, 0.0, 1.0);\n",
    "    vUV = aUV;\n",
    "}\n"
);

const FRAG_ORTHO: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "in vec2 vUV;\n",
    "uniform sampler2D uTex;\n",
    "uniform float uOpacity;\n",
    "uniform vec2 uContentUV;\n", // spritesheet frame × content rect (scale)
    "uniform vec2 uUVOffset;\n", // frame origin inside the buffer
    // WE effects/colorkey (official formula): uKeyColor.rgb = key color,
    // uKeyColor.a = written alpha inside the keyed region.
    "uniform vec4 uKeyColor;\n",
    "uniform float uKeyFuzz;\n",
    "uniform float uKeyTol;\n",
    // WE colorBlendMode via fixed-function blending: the emitted color is
    // shaped so the equation/factors set by the caller compute
    // mix(dst, Blend(dst,src), src.a*uOpacity) exactly (Darken/Multiply/
    // Subtract/Lighten/Screen/Add); transparent texels become no-ops.
    "uniform int uBlendMode;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec4 c = texture(uTex, vUV * uContentUV + uUVOffset);\n",
    "    float delta = dot(abs(uKeyColor.rgb - c.rgb), vec3(1.0));\n",
    "    float blend = smoothstep(0.001, 0.002 + uKeyFuzz, delta - uKeyTol);\n",
    "    c.a *= mix(uKeyColor.a, 1.0, blend);\n",
    "    float o = c.a * uOpacity;\n",
    "    if (uBlendMode == 1 || uBlendMode == 2 || uBlendMode == 5) {\n",
    "        fragColor = vec4(mix(vec3(1.0), c.rgb, o), 1.0);\n", // MIN / dst*src
    "    } else if (uBlendMode == 4 || uBlendMode == 20) {\n",
    "        fragColor = vec4((vec3(1.0) - c.rgb) * o, 0.0);\n", // dst - src
    "    } else if (uBlendMode == 6 || uBlendMode == 7 || uBlendMode == 9\n",
    "               || uBlendMode == 10 || uBlendMode == 31) {\n",
    "        fragColor = vec4(c.rgb * o, 0.0);\n", // MAX / screen / add
    "    } else {\n",
    "        fragColor = vec4(c.rgb, o);\n",
    "    }\n",
    "}\n"
);

/// WE opacity: albedo.a *= mask.r * userAlpha (effects/opacity.frag)
/// uContentUV = (contentW/bufferW, contentH/bufferH) for NPOT-padded masks.
const FRAG_OPACITY: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "in vec2 vUV;\n",
    "uniform sampler2D uTex0;\n",
    "uniform sampler2D uTex1;\n",
    "uniform float uAlpha;\n",
    "uniform vec2 uAlbedoUV;\n", // frame × content scale for albedo
    "uniform vec2 uAlbedoOffset;\n",
    "uniform vec2 uMaskUV;\n",   // content scale for mask (fixes moon glow)
    "out vec4 fragColor;\n",
    "void main() {\n",
    "    vec2 auv = vUV * uAlbedoUV + uAlbedoOffset;\n",
    "    vec2 muv = vUV * uMaskUV;\n",
    "    vec4 albedo = texture(uTex0, auv);\n",
    "    float mask = texture(uTex1, muv).r;\n",
    "    fragColor = vec4(albedo.rgb, albedo.a * mask * uAlpha);\n",
    "}\n"
);

/// util/composelayer sample: for each texel of the layer buffer, look up the
/// backbuffer at the screen position of that point on the layer quad.
/// Matches WE `composelayer.frag` (samples FullFrameBuffer via projected mesh).
/// Drawn with `effect_quad` into a layer-sized FBO (v=0 → layer top content).
const VERT_COMPOSE: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "layout(location=0) in vec2 aPos;\n",
    "layout(location=1) in vec2 aUV;\n",
    "out vec2 vUV;\n",
    "void main() {\n",
    "    vUV = aUV;\n",
    "    gl_Position = vec4(aPos, 0.0, 1.0);\n",
    "}\n"
);

const FRAG_COMPOSE: &str = concat!(
    "#version 300 es\n",
    "precision highp float;\n",
    "in vec2 vUV;\n",
    "uniform sampler2D uFb;\n",
    "uniform vec2 uOrigin;\n", // camera-space center
    "uniform vec2 uSize;\n",   // camera-space extent
    "uniform float uAngle;\n", // camera-space Z angle (= scene angle_z)
    "uniform vec2 uOrtho;\n",
    "uniform vec2 uViewport;\n",
    "out vec4 fragColor;\n",
    "void main() {\n",
    // effect_quad: v=0 at NDC bottom. Layer content wants v=0 at top so that
    // unit-quad draw (top samples v=0) shows upright artwork — same as effect
    // pass ping-pong convention.
    "    vec2 local = vec2((vUV.x - 0.5) * uSize.x, (0.5 - vUV.y) * uSize.y);\n",
    "    float c = cos(uAngle);\n",
    "    float s = sin(uAngle);\n",
    "    vec2 cam = vec2(c * local.x - s * local.y, s * local.x + c * local.y) + uOrigin;\n",
    "    float cover = max(uViewport.x / max(uOrtho.x, 1.0), uViewport.y / max(uOrtho.y, 1.0));\n",
    "    vec2 screen = vec2(uViewport.x * 0.5 + cam.x * cover,\n",
    "                      uViewport.y * 0.5 - cam.y * cover);\n",
    // glCopyTexImage2D stores window bottom-left at tex (0,0).
    "    vec2 fbUV = vec2(screen.x / max(uViewport.x, 1.0),\n",
    "                    1.0 - screen.y / max(uViewport.y, 1.0));\n",
    "    fragColor = texture(uFb, fbUV);\n",
    "}\n"
);

// Waterflow shaders live in wallengine_we::glsl — use at link time via helper below.

// EGL_IMG_context_priority
const EGL_CONTEXT_PRIORITY_LEVEL_IMG: egl::Int = 0x3100;
const EGL_CONTEXT_PRIORITY_LOW_IMG: egl::Int = 0x3102;

impl Renderer {
    /// Create shared EGL state (display/context) + compile shader programs.
    pub fn new(wl_display: &wayland_client::Connection) -> Result<Self, String> {
        let raw = wl_display.backend().display_ptr() as *mut std::ffi::c_void;
        Self::new_from_display(raw)
    }

    /// Headless (no Wayland): EGL default display + pbuffer surface. Used by
    /// offline render harnesses to exercise the exact same GL draw path.
    pub fn headless() -> Result<Self, String> {
        Self::new_from_display(khronos_egl::DEFAULT_DISPLAY)
    }

    fn new_from_display(raw: *mut std::ffi::c_void) -> Result<Self, String> {
        let egl = Instance::new(Static);

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
        let prog_we_overlay = link_program(&gl, VERT_SRC, FRAG_WE_OVERLAY)?;
        let prog_particle = link_program(&gl, VERT_PARTICLE, FRAG_PARTICLE)?;
        let prog_ortho = link_program(&gl, VERT_ORTHO, FRAG_ORTHO)?;
        let prog_compose = link_program(&gl, VERT_COMPOSE, FRAG_COMPOSE)?;
        let prog_waterflow = link_program(
            &gl,
            wallengine_we::glsl::waterflow_vert_es(),
            wallengine_we::glsl::waterflow_frag_es(),
        )?;
        let prog_opacity = link_program(&gl, VERT_ORTHO, FRAG_OPACITY)?;
        let prog_psprite = link_program(&gl, VERT_PSPRITE, FRAG_PSPRITE)?;

        let (psprite_vbo, psprite_vao) = unsafe {
            let vao = gl.create_vertex_array().map_err(|e| format!("vao: {e}"))?;
            let vbo = gl.create_buffer().map_err(|e| format!("vbo: {e}"))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            // layout: x y u v alpha r g b (8 f32)
            let stride = (8 * std::mem::size_of::<f32>()) as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 2 * 4);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 4 * 4);
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 3, glow::FLOAT, false, stride, 5 * 4);
            gl.bind_vertex_array(None);
            (vbo, vao)
        };

        let (particle_vbo, particle_vao) = unsafe {
            let vao = gl.create_vertex_array().map_err(|e| format!("vao: {e}"))?;
            let vbo = gl.create_buffer().map_err(|e| format!("vbo: {e}"))?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            // layout: x y size alpha r g b  (7 f32)
            let stride = (7 * std::mem::size_of::<f32>()) as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 1, glow::FLOAT, false, stride, 2 * 4);
            gl.enable_vertex_attrib_array(2);
            gl.vertex_attrib_pointer_f32(2, 1, glow::FLOAT, false, stride, 3 * 4);
            gl.enable_vertex_attrib_array(3);
            gl.vertex_attrib_pointer_f32(3, 3, glow::FLOAT, false, stride, 4 * 4);
            gl.bind_vertex_array(None);
            (vbo, vao)
        };

        // Unit quad in camera space (y-up). Decoder row0 = image top = v=0.
        // Top of geometry (+y) samples v=0. Content UV scale handles NPOT padding
        // (content lives in the top-left of the buffer — do NOT flip uploads).
        let (quad_vbo, quad_vao) = unsafe {
            let vao = gl.create_vertex_array().map_err(|e| format!("quad vao: {e}"))?;
            let vbo = gl.create_buffer().map_err(|e| format!("quad vbo: {e}"))?;
            #[rustfmt::skip]
            let verts: [f32; 16] = [
                // pos.x  pos.y   u    v   (triangle strip: TL, TR, BL, BR)
                -0.5,  0.5, 0.0, 0.0,
                 0.5,  0.5, 1.0, 0.0,
                -0.5, -0.5, 0.0, 1.0,
                 0.5, -0.5, 1.0, 1.0,
            ];
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_bytes(&verts), glow::STATIC_DRAW);
            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 2 * 4);
            gl.bind_vertex_array(None);
            (vbo, vao)
        };

        // Clip-space fullscreen quad for effect passes. UV convention matches
        // the historical half-quad * scale(2, -2) mapping so FBO sampling stays
        // top-left origin when later drawn as a layer texture:
        //   NDC (-1,-1) ← UV (0,0),  NDC (1,1) ← UV (1,1).
        let (effect_quad_vbo, effect_quad_vao) = unsafe {
            let vao = gl
                .create_vertex_array()
                .map_err(|e| format!("effect quad vao: {e}"))?;
            let vbo = gl
                .create_buffer()
                .map_err(|e| format!("effect quad vbo: {e}"))?;
            #[rustfmt::skip]
            let verts: [f32; 16] = [
                // pos.x  pos.y   u    v   (triangle strip)
                -1.0, -1.0, 0.0, 0.0,
                 1.0, -1.0, 1.0, 0.0,
                -1.0,  1.0, 0.0, 1.0,
                 1.0,  1.0, 1.0, 1.0,
            ];
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_bytes(&verts), glow::STATIC_DRAW);
            let stride = (4 * std::mem::size_of::<f32>()) as i32;
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, stride, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, stride, 2 * 4);
            gl.bind_vertex_array(None);
            (vbo, vao)
        };

        let u = |prog: glow::Program, name: &str| unsafe { gl.get_uniform_location(prog, name) };
        Ok(Renderer {
            egl,
            display,
            context,
            surface,
            window_surfaces: HashMap::new(),
            current_window: None,
            u_tex: u(prog_blit, "uTex"),
            u_scale: u(prog_blit, "uScale"),
            u_offset: u(prog_blit, "uOffset"),
            u_old: u(prog_wipe, "uOld"),
            u_new: u(prog_wipe, "uNew"),
            u_old_scale: u(prog_wipe, "uOldScale"),
            u_new_scale: u(prog_wipe, "uNewScale"),
            u_progress: u(prog_wipe, "uProgress"),
            u_feather: u(prog_wipe, "uFeather"),
            u_size: u(prog_wipe, "uSize"),
            u_weo_old: u(prog_we_overlay, "uOld"),
            u_weo_progress: u(prog_we_overlay, "uProgress"),
            u_weo_feather: u(prog_we_overlay, "uFeather"),
            u_weo_size: u(prog_we_overlay, "uSize"),
            u_part_viewport: u(prog_particle, "uViewport"),
            particle_vbo,
            particle_vao,
            u_psprite_tex: u(prog_psprite, "uTex"),
            u_psprite_overbright: u(prog_psprite, "uOverbright"),
            prog_psprite,
            psprite_vbo,
            psprite_vao,
            prog_ortho,
            u_ortho_mvp: u(prog_ortho, "uMVP"),
            u_ortho_tex: u(prog_ortho, "uTex"),
            u_ortho_opacity: u(prog_ortho, "uOpacity"),
            u_ortho_content_uv: u(prog_ortho, "uContentUV"),
            u_ortho_uv_offset: u(prog_ortho, "uUVOffset"),
            u_ortho_key_color: u(prog_ortho, "uKeyColor"),
            u_ortho_key_fuzz: u(prog_ortho, "uKeyFuzz"),
            u_ortho_key_tol: u(prog_ortho, "uKeyTol"),
            u_ortho_blend_mode: u(prog_ortho, "uBlendMode"),
            prog_waterflow,
            u_wf_mvp: u(prog_waterflow, "uMVP"),
            u_wf_time: u(prog_waterflow, "g_Time"),
            u_wf_speed: u(prog_waterflow, "g_FlowSpeed"),
            u_wf_feather: u(prog_waterflow, "g_PhaseFeather"),
            u_wf_amp: u(prog_waterflow, "g_FlowAmp"),
            u_wf_phase_scale: u(prog_waterflow, "g_FlowPhaseScale"),
            u_wf_tex0: u(prog_waterflow, "g_Texture0"),
            u_wf_tex1: u(prog_waterflow, "g_Texture1"),
            u_wf_tex2: u(prog_waterflow, "g_Texture2"),
            u_wf_tex1_res: u(prog_waterflow, "g_Texture1Resolution"),
            u_wf_frame: u(prog_waterflow, "g_FrameWindow"),
            prog_opacity,
            u_op_mvp: u(prog_opacity, "uMVP"),
            u_op_tex0: u(prog_opacity, "uTex0"),
            u_op_tex1: u(prog_opacity, "uTex1"),
            u_op_alpha: u(prog_opacity, "uAlpha"),
            u_op_albedo_uv: u(prog_opacity, "uAlbedoUV"),
            u_op_albedo_offset: u(prog_opacity, "uAlbedoOffset"),
            u_op_mask_uv: u(prog_opacity, "uMaskUV"),
            quad_vbo,
            quad_vao,
            effect_quad_vbo,
            effect_quad_vao,
            prog_compose,
            u_compose_fb: u(prog_compose, "uFb"),
            u_compose_origin: u(prog_compose, "uOrigin"),
            u_compose_size: u(prog_compose, "uSize"),
            u_compose_angle: u(prog_compose, "uAngle"),
            u_compose_ortho: u(prog_compose, "uOrtho"),
            u_compose_viewport: u(prog_compose, "uViewport"),
            draw_home: std::cell::Cell::new(None),
            gl,
            prog_blit,
            prog_wipe,
            prog_we_overlay,
            prog_particle,
        })
    }

    /// Replace the current surface with a pbuffer of the given size (headless renders).
    pub fn make_pbuffer(&mut self, w: i32, h: i32) -> Result<(), String> {
        let config = pick_config(&self.egl, self.display, egl::PBUFFER_BIT)?;
        let surface = unsafe {
            self.egl.create_pbuffer_surface(
                self.display,
                config,
                &[egl::WIDTH, w, egl::HEIGHT, h, egl::NONE],
            )
        }
        .map_err(|e| format!("pbuffer {w}x{h}: {e:?}"))?;
        self.egl
            .make_current(self.display, Some(surface), Some(surface), Some(self.context))
            .map_err(|e| format!("make_current(pbuffer): {e:?}"))?;
        let old = self.surface;
        self.surface = surface;
        self.current_window = None;
        // Don't destroy cached window surfaces — only orphan pbuffer/old non-window.
        if !self.window_surfaces.values().any(|&s| s == old) {
            self.egl.destroy_surface(self.display, old).ok();
        }
        Ok(())
    }

    /// Point EGL at an output's wl_egl_window (already sized).
    ///
    /// Surfaces are cached per native window. Creating + destroying an EGL
    /// window surface every frame freezes video wallpaper (black) on Mesa/AMD.
    pub fn attach_window(&mut self, window: &wayland_egl::WlEglSurface) -> Result<(), String> {
        let key = window.ptr() as usize;
        if self.current_window == Some(key) {
            // Already bound — just make current (after pbuffer / other ops).
            return self.make_current();
        }
        if let Some(&surf) = self.window_surfaces.get(&key) {
            self.egl
                .make_current(self.display, Some(surf), Some(surf), Some(self.context))
                .map_err(|e| format!("make_current(window cached): {e:?}"))?;
            self.surface = surf;
            self.current_window = Some(key);
            return Ok(());
        }
        let config = pick_config(&self.egl, self.display, egl::WINDOW_BIT)?;
        let surface = unsafe {
            self.egl
                .create_window_surface(self.display, config, window.ptr() as egl::NativeWindowType, None)
        }
        .map_err(|e| format!("create_window_surface: {e:?}"))?;
        self.egl
            .make_current(self.display, Some(surface), Some(surface), Some(self.context))
            .map_err(|e| format!("make_current(window): {e:?}"))?;
        // Keep the previous surface alive if it is a cached window surface;
        // only replace `self.surface` without destroying cached entries.
        self.window_surfaces.insert(key, surface);
        self.surface = surface;
        self.current_window = Some(key);
        Ok(())
    }

    /// Re-bind the current EGL surface/context. Required before any GL work that
    /// is not nested inside `attach_window` / `begin_frame` (video pumps, texture
    /// upload during IPC). Without this, mpv FBO creation can land on a non-current
    /// context and stay pure black forever.
    pub fn make_current(&self) -> Result<(), String> {
        self.egl
            .make_current(
                self.display,
                Some(self.surface),
                Some(self.surface),
                Some(self.context),
            )
            .map_err(|e| format!("make_current: {e:?}"))
    }

    pub fn swap(&self) {
        self.egl.swap_buffers(self.display, self.surface).ok();
    }

    /// Upload RGBA pixels as a GL texture (row 0 = top of image, as decoders provide).
    pub fn upload_rgba(&self, pixels: &[u8], w: u32, h: u32) -> glow::Texture {
        self.upload_rgba_ex(pixels, w, h, false, true)
    }

    /// Video-layer texture: no mipmaps (rebuilt every frame; mip gen was a major cost).
    pub fn upload_rgba_video(&self, pixels: &[u8], w: u32, h: u32) -> glow::Texture {
        self.upload_rgba_ex(pixels, w, h, false, false)
    }

    /// Upload RGBA for **WE scene** sampling: vertically flip so WE UV convention
    /// (v=0 bottom, v=1 top of geometry) shows the image upright — same as LWE’s
    /// texcoord layout used by waterflow and other effect shaders.
    pub fn upload_rgba_we(&self, pixels: &[u8], w: u32, h: u32) -> glow::Texture {
        self.upload_rgba_ex(pixels, w, h, true, true)
    }

    /// Replace pixels in an existing texture (same size). Prefer this for video —
    /// avoids delete/create and mipmap rebuild every frame.
    pub fn update_rgba(&self, tex: glow::Texture, pixels: &[u8], w: u32, h: u32) {
        let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if pixels.len() < need || w == 0 || h == 0 {
            return;
        }
        let gl = &self.gl;
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 4);
            gl.tex_sub_image_2d(
                glow::TEXTURE_2D,
                0,
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(Some(&pixels[..need])),
            );
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    fn upload_rgba_ex(
        &self,
        pixels: &[u8],
        w: u32,
        h: u32,
        flip_y: bool,
        mipmaps: bool,
    ) -> glow::Texture {
        let gl = &self.gl;
        let tex = unsafe { gl.create_texture().expect("create texture") };
        let flipped;
        let slice: &[u8] = if flip_y && w > 0 && h > 1 {
            flipped = flip_rgba_vertical(pixels, w, h);
            &flipped
        } else {
            flipped = Vec::new();
            pixels
        };
        let _ = &flipped;
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
                glow::PixelUnpackData::Slice(Some(slice)),
            );
            // Historical: static uploads generated mipmaps but filtered LINEAR
            // (mips unused). Keep that for static; video skips gen entirely.
            if mipmaps {
                gl.generate_mipmap(glow::TEXTURE_2D);
            }
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_MIN_FILTER,
                glow::LINEAR as i32,
            );
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
        self.draw_blit_layer_offset(vw, vh, tex, img_w, img_h, fit, opacity, (0.0, 0.0));
    }

    pub fn draw_blit_layer_offset(
        &self,
        vw: i32,
        vh: i32,
        tex: glow::Texture,
        img_w: u32,
        img_h: u32,
        fit: crate::config::FitMode,
        opacity: f32,
        uv_offset: (f32, f32),
    ) {
        self.draw_blit_present(
            vw, vh, tex, img_w, img_h, fit, 1.0, uv_offset, false, false,
        );
        let _ = opacity;
    }

    /// Fullscreen blit with fit + zoom + pan + optional flips (WE presentation).
    pub fn draw_blit_present(
        &self,
        vw: i32,
        vh: i32,
        tex: glow::Texture,
        img_w: u32,
        img_h: u32,
        fit: crate::config::FitMode,
        zoom: f32,
        uv_offset: (f32, f32),
        flip_h: bool,
        flip_v: bool,
    ) {
        let gl = &self.gl;
        let zoom = zoom.clamp(0.25, 4.0);
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_blit));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_tex.as_ref(), 0);
            let (mut sx, mut sy) = fit_uv_scale(vw, vh, img_w, img_h, fit);
            // Zoom in → sample a smaller UV window.
            sx /= zoom;
            sy /= zoom;
            if flip_h {
                sx = -sx;
            }
            if flip_v {
                sy = -sy;
            }
            gl.uniform_2_f32(self.u_scale.as_ref(), sx, sy);
            gl.uniform_2_f32(self.u_offset.as_ref(), uv_offset.0, uv_offset.1);
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
        let mut data = Vec::with_capacity(particles.len() * 7);
        for p in particles {
            data.push(p.x);
            data.push(p.y);
            data.push(p.size);
            data.push(p.alpha * opacity);
            data.push(1.0);
            data.push(1.0);
            data.push(1.0);
        }
        self.draw_points(vw, vh, &data);
    }

    /// Draw WE particles. `data` layout: x_norm y_norm size_norm alpha r g b (7 floats).
    /// `additive`: snow/embers use SRC_ALPHA, ONE; smoke uses standard alpha blend.
    pub fn draw_points(&self, vw: i32, vh: i32, data: &[f32]) {
        self.draw_points_ex(vw, vh, data, true);
    }

    pub fn draw_points_ex(&self, vw: i32, vh: i32, data: &[f32], additive: bool) {
        if data.is_empty() || vw <= 0 || vh <= 0 {
            return;
        }
        // Support both legacy 4-float and colored 7-float layouts.
        let stride = if data.len() % 7 == 0 {
            7
        } else if data.len() % 4 == 0 {
            4
        } else {
            return;
        };
        let count = (data.len() / stride) as i32;
        if count <= 0 {
            return;
        }
        // Expand 4-float → 7-float with white if needed
        let owned: Vec<f32>;
        let slice: &[f32] = if stride == 4 {
            owned = data
                .chunks(4)
                .flat_map(|c| [c[0], c[1], c[2], c[3], 1.0, 1.0, 1.0])
                .collect();
            &owned
        } else {
            data
        };
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_particle));
            gl.uniform_2_f32(self.u_part_viewport.as_ref(), vw as f32, vh as f32);
            gl.bind_vertex_array(Some(self.particle_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.particle_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_bytes(slice), glow::STREAM_DRAW);
            gl.enable(glow::BLEND);
            if additive {
                gl.blend_func(glow::SRC_ALPHA, glow::ONE);
            } else {
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            }
            gl.draw_arrays(glow::POINTS, 0, count);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.bind_vertex_array(None);
        }
    }

    /// Bind an offscreen colour target (or the window when `None`).
    /// When `home` is true, also record this as the restore target for effect
    /// passes / compose sampling for the rest of the frame.
    pub fn bind_draw_target(
        &self,
        target: Option<(glow::Framebuffer, u32, u32)>,
        default_vw: i32,
        default_vh: i32,
        home: bool,
    ) {
        let gl = &self.gl;
        if home {
            self.draw_home.set(target);
        }
        unsafe {
            match target {
                Some((fbo, w, h)) => {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
                    gl.viewport(0, 0, w as i32, h as i32);
                }
                None => {
                    gl.bind_framebuffer(glow::FRAMEBUFFER, None);
                    gl.viewport(0, 0, default_vw, default_vh);
                }
            }
        }
    }

    /// Re-bind the frame's home draw target (scene RT or window).
    pub fn restore_draw_home(&self, default_vw: i32, default_vh: i32) {
        let home = self.draw_home.get();
        self.bind_draw_target(home, default_vw, default_vh, false);
    }

    /// Clear the currently bound draw buffer.
    pub fn clear_bound(&self, color: [f32; 4]) {
        let gl = &self.gl;
        unsafe {
            gl.clear_color(color[0], color[1], color[2], color[3]);
            gl.clear(glow::COLOR_BUFFER_BIT);
        }
    }

    /// Present scene RT → window via glBlitFramebuffer (no UV flip surprises).
    pub fn blit_fbo_to_default(&self, src: glow::Framebuffer, w: u32, h: u32) {
        let gl = &self.gl;
        let (w, h) = (w as i32, h as i32);
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(src));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
            gl.viewport(0, 0, w, h);
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        self.draw_home.set(None);
    }

    /// Copy colour from `src` FBO into `dst` FBO (same size). Used to snapshot
    /// the scene RT before composelayer samples it — avoids sampling a texture
    /// that is still attached to an FBO (Mesa often returns black for that).
    pub fn blit_fbo_to_fbo(
        &self,
        src: glow::Framebuffer,
        dst: glow::Framebuffer,
        w: u32,
        h: u32,
    ) {
        let gl = &self.gl;
        let (w, h) = (w as i32, h as i32);
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(src));
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(dst));
            gl.blit_framebuffer(
                0,
                0,
                w,
                h,
                0,
                0,
                w,
                h,
                glow::COLOR_BUFFER_BIT,
                glow::NEAREST,
            );
        }
        // Return to the frame home so subsequent draws land correctly.
        self.restore_draw_home(w, h);
    }

    /// Fill `target` (layer-sized) by sampling `fb` at the screen positions of
    /// this layer's rotated quad — the util/composelayer capture step.
    /// Restores the frame home draw target afterwards.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_compose_layer(
        &self,
        target: glow::Framebuffer,
        tw: u32,
        th: u32,
        fb: glow::Texture,
        origin: [f32; 2],
        size: [f32; 2],
        angle_z_screen: f32,
        ortho_w: f32,
        ortho_h: f32,
        vw: i32,
        vh: i32,
    ) {
        let gl = &self.gl;
        // Scene and camera space use the same Y-up rotation.
        let angle = angle_z_screen;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target));
            gl.viewport(0, 0, tw as i32, th as i32);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.disable(glow::BLEND);
            gl.use_program(Some(self.prog_compose));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(fb));
            if let Some(l) = self.u_compose_fb.as_ref() {
                gl.uniform_1_i32(Some(l), 0);
            }
            if let Some(l) = self.u_compose_origin.as_ref() {
                gl.uniform_2_f32(Some(l), origin[0], origin[1]);
            }
            if let Some(l) = self.u_compose_size.as_ref() {
                gl.uniform_2_f32(Some(l), size[0].abs().max(1.0), size[1].abs().max(1.0));
            }
            if let Some(l) = self.u_compose_angle.as_ref() {
                gl.uniform_1_f32(Some(l), angle);
            }
            if let Some(l) = self.u_compose_ortho.as_ref() {
                gl.uniform_2_f32(Some(l), ortho_w.max(1.0), ortho_h.max(1.0));
            }
            if let Some(l) = self.u_compose_viewport.as_ref() {
                gl.uniform_2_f32(Some(l), vw.max(1) as f32, vh.max(1) as f32);
            }
            gl.bind_vertex_array(Some(self.effect_quad_vao));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            gl.enable(glow::BLEND);
        }
        self.restore_draw_home(vw, vh);
    }

    /// Off-screen colour target for effect passes.
    pub fn create_target(&self, w: u32, h: u32) -> Option<(glow::Framebuffer, glow::Texture)> {
        let gl = &self.gl;
        unsafe {
            let tex = gl.create_texture().ok()?;
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
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_S,
                glow::CLAMP_TO_EDGE as i32,
            );
            gl.tex_parameter_i32(
                glow::TEXTURE_2D,
                glow::TEXTURE_WRAP_T,
                glow::CLAMP_TO_EDGE as i32,
            );
            let fbo = gl.create_framebuffer().ok()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(
                glow::FRAMEBUFFER,
                glow::COLOR_ATTACHMENT0,
                glow::TEXTURE_2D,
                Some(tex),
                0,
            );
            let ok = gl.check_framebuffer_status(glow::FRAMEBUFFER) == glow::FRAMEBUFFER_COMPLETE;
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
            gl.bind_texture(glow::TEXTURE_2D, None);
            if !ok {
                gl.delete_framebuffer(fbo);
                gl.delete_texture(tex);
                return None;
            }
            Some((fbo, tex))
        }
    }

    pub fn delete_target(&self, fbo: glow::Framebuffer, tex: glow::Texture) {
        unsafe {
            self.gl.delete_framebuffer(fbo);
            self.gl.delete_texture(tex);
        }
    }

    /// Detach: keep the colour texture alive for sampling, free only the FBO.
    /// Used by the WE wipe still — a texture left attached to a live FBO
    /// samples black on Mesa.
    pub fn release_target(&self, fbo: glow::Framebuffer, _tex: glow::Texture) {
        unsafe {
            self.gl.delete_framebuffer(fbo);
        }
    }

    /// Read back RGBA8 from a bound colour attachment FBO (editor GPU preview).
    pub fn read_rgba(&self, fbo: glow::Framebuffer, w: u32, h: u32) -> Vec<u8> {
        self.read_rgba_from(Some(fbo), w, h)
    }

    /// Read the currently bound draw buffer (window / default FB when `fbo` is None).
    /// Used for boot-still capture after a normal wallpaper frame is drawn.
    pub fn read_rgba_from(&self, fbo: Option<glow::Framebuffer>, w: u32, h: u32) -> Vec<u8> {
        let mut buf = vec![0u8; (w * h * 4) as usize];
        let gl = &self.gl;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.read_pixels(
                0,
                0,
                w as i32,
                h as i32,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut buf)),
            );
            gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        }
        // GL origin is bottom-left; flip vertically for image/UI top-left.
        let row = (w * 4) as usize;
        let mut flipped = vec![0u8; buf.len()];
        for y in 0..h as usize {
            let src = (h as usize - 1 - y) * row;
            let dst = y * row;
            flipped[dst..dst + row].copy_from_slice(&buf[src..src + row]);
        }
        flipped
    }

    /// Copy the content rectangle out of padded TEX storage without changing
    /// its row orientation (both textures retain the image's v=0 top row).
    pub fn copy_texture_content(&self, source: glow::Texture, target: glow::Framebuffer, w: u32, h: u32) -> Result<(), String> {
        unsafe {
            let gl = &self.gl;
            let read = gl.create_framebuffer()?;
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(read));
            gl.framebuffer_texture_2d(glow::READ_FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(source), 0);
            gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(target));
            gl.blit_framebuffer(0, 0, w as i32, h as i32, 0, 0, w as i32, h as i32, glow::COLOR_BUFFER_BIT, glow::NEAREST);
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, None);
            gl.delete_framebuffer(read);
        }
        self.restore_draw_home(w as i32, h as i32);
        Ok(())
    }

    /// Compile a translated WE effect pass. Returns the linked program.
    pub fn compile_effect(&self, vert: &str, frag: &str) -> Result<glow::Program, String> {
        link_program(&self.gl, vert, frag)
    }

    /// Blit `src` into the bound-by-caller target through an effect program.
    /// `extra` are (unit, texture, w, h) for g_Texture1..N; `uniforms` are already
    /// resolved name→value pairs from the material/scene constants.
    ///
    /// `src_w`/`src_h` are the **sampled** texture0 dimensions (may differ from
    /// the target when a pass downsamples). WE shaders read
    /// `g_Texture0Resolution` for aspect and UV math — feeding the target size
    /// here breaks multipass chains (godrays, bloom, localcontrast, …).
    #[allow(clippy::too_many_arguments)]
    pub fn run_effect_pass(
        &self,
        prog: glow::Program,
        target: glow::Framebuffer,
        w: u32,
        h: u32,
        src: glow::Texture,
        src_w: u32,
        src_h: u32,
        src_uv: (f32, f32),
        extra: &[(u32, glow::Texture, u32, u32, (f32, f32))],
        uniforms: &[(String, wallengine_we::EffectValue)],
        time: f32,
        // Cursor in 0..1 wallpaper UV (WE `g_PointerPosition`).
        pointer: [f32; 2],
    ) {
        let gl = &self.gl;
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(target));
            gl.viewport(0, 0, w as i32, h as i32);
            gl.clear_color(0.0, 0.0, 0.0, 0.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.use_program(Some(prog));

            // Identity MVP: effect mesh is already in clip space (see
            // `effect_quad_vao`). Shaders that apply the matrix and shaders
            // that write `gl_Position = vec4(a_Position, 1.0)` both work.
            #[rustfmt::skip]
            let ident4: [f32; 16] = [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ];
            if let Some(l) = gl.get_uniform_location(prog, "g_ModelViewProjectionMatrix") {
                gl.uniform_matrix_4_f32_slice(Some(&l), false, &ident4);
            }
            if let Some(l) = gl.get_uniform_location(prog, "g_EffectTextureProjectionMatrixInverse")
            {
                gl.uniform_matrix_4_f32_slice(Some(&l), false, &ident4);
            }
            if let Some(l) = gl.get_uniform_location(prog, "g_EffectModelViewProjectionMatrix") {
                gl.uniform_matrix_4_f32_slice(Some(&l), false, &ident4);
            }
            for name in ["g_Time", "g_GlobalTime", "g_AnimationTime"] {
                if let Some(l) = gl.get_uniform_location(prog, name) {
                    gl.uniform_1_f32(Some(&l), time);
                }
            }
            // Texel size is in *target* space (blur offsets, etc.).
            if let Some(l) = gl.get_uniform_location(prog, "g_TexelSize") {
                gl.uniform_2_f32(Some(&l), 1.0 / w.max(1) as f32, 1.0 / h.max(1) as f32);
            }
            // Texture0 resolution describes the *source* being sampled.
            let (sw, sh) = (src_w.max(1) as f32, src_h.max(1) as f32);
            if let Some(l) = gl.get_uniform_location(prog, "g_Texture0Resolution") {
                gl.uniform_4_f32(Some(&l), sw, sh, sw * src_uv.0, sh * src_uv.1);
            }
            if let Some(l) = gl.get_uniform_location(prog, "g_PointerPosition") {
                gl.uniform_2_f32(
                    Some(&l),
                    pointer[0].clamp(0.0, 1.0),
                    pointer[1].clamp(0.0, 1.0),
                );
            }
            // Silent audio spectrum (arrays default to zero, so only sizes matter).
            for name in ["g_AudioSpectrum16Left", "g_AudioSpectrum16Right"] {
                if let Some(l) = gl.get_uniform_location(prog, name) {
                    gl.uniform_1_f32_slice(Some(&l), &[0.0f32; 16]);
                }
            }

            for (name, val) in uniforms {
                let Some(l) = gl.get_uniform_location(prog, name) else {
                    continue;
                };
                match val {
                    wallengine_we::EffectValue::Float(f) => gl.uniform_1_f32(Some(&l), *f),
                    wallengine_we::EffectValue::Vec2(v) => gl.uniform_2_f32(Some(&l), v[0], v[1]),
                    wallengine_we::EffectValue::Vec3(v) => {
                        gl.uniform_3_f32(Some(&l), v[0], v[1], v[2])
                    }
                    wallengine_we::EffectValue::Vec4(v) => {
                        gl.uniform_4_f32(Some(&l), v[0], v[1], v[2], v[3])
                    }
                    wallengine_we::EffectValue::String(_) => {}
                    // Should already be resolved to Float before draw.
                    wallengine_we::EffectValue::Scripted { default, .. } => {
                        gl.uniform_1_f32(Some(&l), *default)
                    }
                }
            }

            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(src));
            if let Some(l) = gl.get_uniform_location(prog, "g_Texture0") {
                gl.uniform_1_i32(Some(&l), 0);
            }
            for (slot, tex, tw, th, uv) in extra {
                gl.active_texture(glow::TEXTURE0 + slot);
                gl.bind_texture(glow::TEXTURE_2D, Some(*tex));
                if let Some(l) = gl.get_uniform_location(prog, &format!("g_Texture{slot}")) {
                    gl.uniform_1_i32(Some(&l), *slot as i32);
                }
                if let Some(l) =
                    gl.get_uniform_location(prog, &format!("g_Texture{slot}Resolution"))
                {
                    gl.uniform_4_f32(Some(&l), *tw as f32, *th as f32, *tw as f32 * uv.0, *th as f32 * uv.1);
                }
            }

            gl.disable(glow::BLEND);
            gl.bind_vertex_array(Some(self.effect_quad_vao));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.enable(glow::BLEND);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
        // Always return to the scene RT / window — never leave a pass FBO bound
        // or FRAMEBUFFER=None when the frame home is an offscreen target.
        // (default vw/vh only matter when home is None.)
        self.restore_draw_home(w as i32, h as i32);
    }

    /// Draw textured particle sprites. `verts` is CPU-expanded triangles:
    /// x y u v alpha r g b per vertex (6 verts per particle).
    pub fn draw_particle_sprites(
        &self,
        vw: i32,
        vh: i32,
        tex: glow::Texture,
        verts: &[f32],
        additive: bool,
        overbright: f32,
    ) {
        if verts.is_empty() || vw <= 0 || vh <= 0 {
            return;
        }
        let count = (verts.len() / 8) as i32;
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_psprite));
            gl.uniform_1_f32(self.u_psprite_overbright.as_ref(), overbright.max(0.0));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_psprite_tex.as_ref(), 0);
            gl.bind_vertex_array(Some(self.psprite_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.psprite_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_bytes(verts), glow::STREAM_DRAW);
            gl.enable(glow::BLEND);
            if additive {
                gl.blend_func(glow::SRC_ALPHA, glow::ONE);
            } else {
                gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            }
            gl.draw_arrays(glow::TRIANGLES, 0, count);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Draw a textured quad centered at `origin` (**camera space**, y-up) with `size`.
    /// `angle_z` is the authored Y-up Z rotation from scene.json.
    /// `content_uv` scales UVs into the content rect of an NPOT-padded texture.
    pub fn draw_ortho_image(
        &self,
        vw: i32,
        vh: i32,
        ortho_w: f32,
        ortho_h: f32,
        tex: glow::Texture,
        origin: [f32; 2],
        size: [f32; 2],
        angle_z: f32,
        opacity: f32,
        content_uv: (f32, f32),
        uv_offset: (f32, f32),
        key: Option<wallengine_we::ColorkeyParams>,
        blend_mode: i32,
    ) {
        let base = wallengine_we::camera_to_ndc_mvp(ortho_w, ortho_h, vw, vh);
        let mvp = wallengine_we::mul_mat4(
            base,
            wallengine_we::model_center_camera(origin, size, angle_z),
        );
        // No key: written alpha 1 (no change) and tolerance -10 (blend always 1).
        let (kc, ka, kf, kt) = match key {
            Some(k) => (k.color, k.alpha, k.fuzziness, k.tolerance),
            None => ([0.0f32, 0.0, 0.0], 1.0, 0.0, -10.0),
        };
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_ortho));
            gl.uniform_matrix_4_f32_slice(self.u_ortho_mvp.as_ref(), false, &mvp);
            gl.uniform_1_f32(self.u_ortho_opacity.as_ref(), opacity.clamp(0.0, 1.0));
            gl.uniform_4_f32(self.u_ortho_key_color.as_ref(), kc[0], kc[1], kc[2], ka);
            gl.uniform_1_f32(self.u_ortho_key_fuzz.as_ref(), kf);
            gl.uniform_1_f32(self.u_ortho_key_tol.as_ref(), kt);
            gl.uniform_2_f32(
                self.u_ortho_content_uv.as_ref(),
                content_uv.0.max(0.001),
                content_uv.1.max(0.001),
            );
            gl.uniform_2_f32(self.u_ortho_uv_offset.as_ref(), uv_offset.0, uv_offset.1);
            gl.uniform_1_i32(self.u_ortho_blend_mode.as_ref(), blend_mode);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_ortho_tex.as_ref(), 0);
            gl.bind_vertex_array(Some(self.quad_vao));
            gl.enable(glow::BLEND);
            // colorBlendMode → fixed-function equivalents (dst alpha preserved);
            // modes without one (Overlay/SoftLight/…) fall back to normal, and
            // the shader's else-branch emission matches.
            set_image_blend(gl, blend_mode);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.blend_equation(glow::FUNC_ADD);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.bind_vertex_array(None);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Waterflow effect on an image layer (WE waterflow).
    pub fn draw_waterflow(
        &self,
        vw: i32,
        vh: i32,
        ortho_w: f32,
        ortho_h: f32,
        albedo: glow::Texture,
        mask: glow::Texture,
        phase: glow::Texture,
        mask_w: u32,
        mask_h: u32,
        albedo_w: u32,
        albedo_h: u32,
        origin: [f32; 2],
        size: [f32; 2],
        angle_z: f32,
        time: f32,
        speed: f32,
        strength: f32,
        phasescale: f32,
        feather: f32,
        uv_scale: (f32, f32),
        uv_offset: (f32, f32),
    ) {
        let base = wallengine_we::camera_to_ndc_mvp(ortho_w, ortho_h, vw, vh);
        let mvp = wallengine_we::mul_mat4(
            base,
            wallengine_we::model_center_camera(origin, size, angle_z),
        );
        let gl = &self.gl;
        // g_Texture1Resolution = (texW, texH, realW, realH) — flow UV scale
        let tres = [
            mask_w as f32,
            mask_h as f32,
            albedo_w as f32,
            albedo_h as f32,
        ];
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_waterflow));
            gl.uniform_matrix_4_f32_slice(self.u_wf_mvp.as_ref(), false, &mvp);
            gl.uniform_1_f32(self.u_wf_time.as_ref(), time);
            gl.uniform_1_f32(self.u_wf_speed.as_ref(), speed);
            gl.uniform_1_f32(self.u_wf_feather.as_ref(), feather.clamp(0.05, 0.5));
            gl.uniform_1_f32(self.u_wf_amp.as_ref(), strength);
            gl.uniform_1_f32(self.u_wf_phase_scale.as_ref(), phasescale);
            gl.uniform_4_f32(
                self.u_wf_tex1_res.as_ref(),
                tres[0],
                tres[1],
                tres[2],
                tres[3],
            );
            gl.uniform_4_f32(
                self.u_wf_frame.as_ref(),
                uv_offset.0,
                uv_offset.1,
                uv_scale.0.max(0.001),
                uv_scale.1.max(0.001),
            );
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(albedo));
            gl.uniform_1_i32(self.u_wf_tex0.as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(mask));
            gl.uniform_1_i32(self.u_wf_tex1.as_ref(), 1);
            gl.active_texture(glow::TEXTURE2);
            gl.bind_texture(glow::TEXTURE_2D, Some(phase));
            gl.uniform_1_i32(self.u_wf_tex2.as_ref(), 2);
            gl.bind_vertex_array(Some(self.quad_vao));
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// WE opacity mask: `albedo.a *= mask.r * alpha`.
    /// `albedo_uv` / `mask_uv` are content/buffer scales for NPOT-padded .tex files.
    pub fn draw_opacity_masked(
        &self,
        vw: i32,
        vh: i32,
        ortho_w: f32,
        ortho_h: f32,
        albedo: glow::Texture,
        mask: glow::Texture,
        origin: [f32; 2],
        size: [f32; 2],
        angle_z: f32,
        alpha: f32,
        albedo_uv: (f32, f32),
        albedo_offset: (f32, f32),
        mask_uv: (f32, f32),
    ) {
        let base = wallengine_we::camera_to_ndc_mvp(ortho_w, ortho_h, vw, vh);
        let mvp = wallengine_we::mul_mat4(
            base,
            wallengine_we::model_center_camera(origin, size, angle_z),
        );
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_opacity));
            gl.uniform_matrix_4_f32_slice(self.u_op_mvp.as_ref(), false, &mvp);
            gl.uniform_1_f32(self.u_op_alpha.as_ref(), alpha.clamp(0.0, 1.0));
            gl.uniform_2_f32(
                self.u_op_albedo_uv.as_ref(),
                albedo_uv.0.max(0.001),
                albedo_uv.1.max(0.001),
            );
            gl.uniform_2_f32(
                self.u_op_albedo_offset.as_ref(),
                albedo_offset.0,
                albedo_offset.1,
            );
            gl.uniform_2_f32(
                self.u_op_mask_uv.as_ref(),
                mask_uv.0.max(0.001),
                mask_uv.1.max(0.001),
            );
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(albedo));
            gl.uniform_1_i32(self.u_op_tex0.as_ref(), 0);
            gl.active_texture(glow::TEXTURE1);
            gl.bind_texture(glow::TEXTURE_2D, Some(mask));
            gl.uniform_1_i32(self.u_op_tex1.as_ref(), 1);
            gl.bind_vertex_array(Some(self.quad_vao));
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, None);
        }
    }

    /// Draw a puppet mesh: triangles in camera space as (x,y,u,v) quads of verts.
    /// `tris` is a flat list of triangle vertices (3 per triangle), each `[cam_x, cam_y, u, v]`.
    pub fn draw_puppet_tris(
        &self,
        vw: i32,
        vh: i32,
        ortho_w: f32,
        ortho_h: f32,
        tex: glow::Texture,
        tris: &[[f32; 4]],
        content_uv: (f32, f32),
        uv_offset: (f32, f32),
        opacity: f32,
        key: Option<wallengine_we::ColorkeyParams>,
        blend_mode: i32,
    ) {
        if tris.is_empty() || vw <= 0 || vh <= 0 {
            return;
        }
        let mvp = wallengine_we::camera_to_ndc_mvp(ortho_w, ortho_h, vw, vh);
        // Expand to pos.xy + uv.xy interleaved for the ortho program. Puppet verts
        // are already in camera space; we feed them as a triangle list with identity
        // model (bake positions into the VBO, use MVP = projection only).
        // Unit-quad program expects local [-0.5,0.5]; instead use a one-shot path:
        // map each cam-space point through MVP as a point with its UV.
        let gl = &self.gl;
        // Build expanded verts: we reuse particle VBO temporarily for stream upload
        // of (ndc_x, ndc_y, u, v) and a tiny custom draw via ortho with identity-like
        // positions. Simpler: convert each triangle into screen and draw with
        // gl.draw_arrays TRIANGLES using a temporary buffer.
        let a = mvp[0];
        let b = mvp[5];
        let mut data = Vec::with_capacity(tris.len() * 4);
        for v in tris {
            // cam → ndc via diagonal MVP
            let ndc_x = v[0] * a;
            let ndc_y = v[1] * b;
            data.push(ndc_x);
            data.push(ndc_y);
            data.push(v[2] * content_uv.0.max(0.001) + uv_offset.0);
            data.push(v[3] * content_uv.1.max(0.001) + uv_offset.1);
        }
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_ortho));
            // Identity MVP — positions already in NDC
            #[rustfmt::skip]
            let ident: [f32; 16] = [
                1.0, 0.0, 0.0, 0.0,
                0.0, 1.0, 0.0, 0.0,
                0.0, 0.0, 1.0, 0.0,
                0.0, 0.0, 0.0, 1.0,
            ];
            gl.uniform_matrix_4_f32_slice(self.u_ortho_mvp.as_ref(), false, &ident);
            gl.uniform_1_f32(self.u_ortho_opacity.as_ref(), opacity.clamp(0.0, 1.0));
            let (kc, ka, kf, kt) = match key {
                Some(k) => (k.color, k.alpha, k.fuzziness, k.tolerance),
                None => ([0.0; 3], 1.0, 0.0, -10.0),
            };
            gl.uniform_4_f32(self.u_ortho_key_color.as_ref(), kc[0], kc[1], kc[2], ka);
            gl.uniform_1_f32(self.u_ortho_key_fuzz.as_ref(), kf);
            gl.uniform_1_f32(self.u_ortho_key_tol.as_ref(), kt);
            gl.uniform_2_f32(self.u_ortho_uv_offset.as_ref(), 0.0, 0.0);
            gl.uniform_1_i32(self.u_ortho_blend_mode.as_ref(), blend_mode);
            gl.uniform_2_f32(self.u_ortho_content_uv.as_ref(), 1.0, 1.0);
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(tex));
            gl.uniform_1_i32(self.u_ortho_tex.as_ref(), 0);
            gl.bind_vertex_array(Some(self.quad_vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.quad_vbo));
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_bytes(&data), glow::STREAM_DRAW);
            // pos.xy uv.xy stride 16
            gl.enable_vertex_attrib_array(0);
            gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
            gl.enable_vertex_attrib_array(1);
            gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);
            gl.enable(glow::BLEND);
            set_image_blend(gl, blend_mode);
            gl.draw_arrays(glow::TRIANGLES, 0, tris.len() as i32);
            gl.blend_equation(glow::FUNC_ADD);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
            // Restore static unit-quad for subsequent ortho draws
            #[rustfmt::skip]
            let unit: [f32; 16] = [
                -0.5,  0.5, 0.0, 0.0,
                 0.5,  0.5, 1.0, 0.0,
                -0.5, -0.5, 0.0, 1.0,
                 0.5, -0.5, 1.0, 1.0,
            ];
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_bytes(&unit), glow::STATIC_DRAW);
            gl.bind_vertex_array(None);
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

    /// Overlay the outgoing WE still over whatever is already on the framebuffer,
    /// revealed by the same diagonal metric as `draw_wipe`. Blend must be enabled
    /// with premultiplied-compatible alpha (the fragment emits `alpha = mask`).
    pub fn draw_we_overlay(
        &self,
        vw: i32,
        vh: i32,
        old: glow::Texture,
        progress: f32,
        feather_px: f32,
    ) {
        let gl = &self.gl;
        unsafe {
            gl.viewport(0, 0, vw, vh);
            gl.use_program(Some(self.prog_we_overlay));
            gl.active_texture(glow::TEXTURE0);
            gl.bind_texture(glow::TEXTURE_2D, Some(old));
            gl.uniform_1_i32(self.u_weo_old.as_ref(), 0);
            gl.uniform_1_f32(self.u_weo_progress.as_ref(), progress);
            let diag = (vw + vh) as f32;
            gl.uniform_1_f32(self.u_weo_feather.as_ref(), feather_px / diag.max(1.0));
            gl.uniform_2_f32(self.u_weo_size.as_ref(), vw as f32, vh as f32);
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
        // WE effect shaders use `attribute vec3 a_Position` / `a_TexCoord` without
        // layout(location=…). Bind them to the unit-quad VAO slots (0=pos, 1=uv)
        // so every compiled pass samples the full-screen quad correctly — without
        // this, auto-assigned locations miss the VAO and effects draw a static
        // corner texel (looks like "not animated at all").
        gl.bind_attrib_location(prog, 0, "a_Position");
        gl.bind_attrib_location(prog, 0, "aPos");
        gl.bind_attrib_location(prog, 1, "a_TexCoord");
        gl.bind_attrib_location(prog, 1, "aUV");
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

/// Flip RGBA8 image vertically (row 0 becomes last row).
fn flip_rgba_vertical(pixels: &[u8], w: u32, h: u32) -> Vec<u8> {
    let row = (w as usize).saturating_mul(4);
    let h = h as usize;
    let mut out = vec![0u8; row.saturating_mul(h)];
    if row == 0 || h == 0 {
        return out;
    }
    for y in 0..h {
        let src = y * row;
        let dst = (h - 1 - y) * row;
        out[dst..dst + row].copy_from_slice(&pixels[src..src + row]);
    }
    out
}

unsafe fn set_image_blend(gl: &glow::Context, blend_mode: i32) {
            match blend_mode {
                1 | 5 => {
                    gl.blend_equation_separate(glow::MIN, glow::FUNC_ADD);
                    gl.blend_func_separate(glow::ONE, glow::ONE, glow::ZERO, glow::ONE);
                }
                6 | 10 => {
                    gl.blend_equation_separate(glow::MAX, glow::FUNC_ADD);
                    gl.blend_func_separate(glow::ONE, glow::ONE, glow::ZERO, glow::ONE);
                }
                2 => {
                    gl.blend_func_separate(glow::DST_COLOR, glow::ZERO, glow::ZERO, glow::ONE);
                }
                7 => {
                    gl.blend_func_separate(
                        glow::ONE_MINUS_DST_COLOR,
                        glow::ONE,
                        glow::ZERO,
                        glow::ONE,
                    );
                }
                9 | 31 => {
                    gl.blend_func_separate(glow::ONE, glow::ONE, glow::ZERO, glow::ONE);
                }
                4 | 20 => {
                    gl.blend_equation_separate(glow::FUNC_REVERSE_SUBTRACT, glow::FUNC_ADD);
                    gl.blend_func_separate(glow::ONE, glow::ONE, glow::ZERO, glow::ONE);
                }
                _ => {
                    gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);
                }
            }
}
