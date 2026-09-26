//! Requires EGL/GLES (Mesa surfaceless works); all scene assets are synthetic.
use glow::HasContext;
use std::collections::HashMap;
use walld::{
    effects::{EffectTargets, EffectTexture},
    render::Renderer,
};
use wallengine_we::scene::effectpass::{EffectPass, LoadedEffect};

const VERT: &str = "#version 300 es\nlayout(location=0) in vec2 a_Position; layout(location=1) in vec2 a_TexCoord; out vec2 uv; void main(){ uv=a_TexCoord; gl_Position=vec4(a_Position,0,1); }";
fn pass(body: &str) -> EffectPass {
    EffectPass {
        name: "synthetic".into(), vert: VERT.into(),
        frag: format!("#version 300 es\nprecision highp float; in vec2 uv; uniform sampler2D g_Texture0; uniform sampler2D g_Texture1; out vec4 color; void main(){{ {body} }}"),
        uniforms: HashMap::new(), textures: HashMap::new(), blending: "normal".into(),
        combos: HashMap::new(), target: None, binds: vec![],
    }
}
fn effect(file: &str, passes: Vec<EffectPass>) -> LoadedEffect {
    LoadedEffect {
        file: file.into(),
        name: file.into(),
        passes,
        fbos: vec![],
    }
}
fn programs(r: &Renderer, effects: &[LoadedEffect]) -> Vec<Option<glow::Program>> {
    effects
        .iter()
        .flat_map(|e| &e.passes)
        .map(|p| Some(r.compile_effect(&p.vert, &p.frag).unwrap()))
        .collect()
}
fn read(r: &Renderer, image: EffectTexture) -> Vec<u8> {
    unsafe {
        let gl = &r.gl;
        let f = gl.create_framebuffer().unwrap();
        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(f));
        gl.framebuffer_texture_2d(
            glow::FRAMEBUFFER,
            glow::COLOR_ATTACHMENT0,
            glow::TEXTURE_2D,
            Some(image.tex),
            0,
        );
        assert_eq!(
            gl.check_framebuffer_status(glow::FRAMEBUFFER),
            glow::FRAMEBUFFER_COMPLETE
        );
        let mut buf = vec![0; (image.w * image.h * 4) as usize];
        gl.read_pixels(
            0,
            0,
            image.w as i32,
            image.h as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut buf)),
        );
        assert_eq!(gl.get_error(), glow::NO_ERROR);
        gl.bind_framebuffer(glow::FRAMEBUFFER, None);
        gl.delete_framebuffer(f);
        buf
    }
}
fn near(actual: &[u8], expected: &[u8]) {
    for (a, e) in actual.iter().zip(expected) {
        assert!(
            (*a as i32 - *e as i32).abs() <= 2,
            "{actual:?} != {expected:?}"
        );
    }
}

#[test]
fn gpu_effect_chains_and_draw_state_are_stable() {
    let mut r = Renderer::headless().expect("EGL required for scene fidelity regression");
    r.make_pbuffer(8, 8).unwrap();
    let input = EffectTexture {
        tex: r.upload_rgba(&[100, 40, 20, 255].repeat(64), 8, 8),
        w: 8,
        h: 8,
        uv_scale: (1.0, 1.0),
    };
    let identity = "color=texture(g_Texture0,uv);";
    let half = "color=texture(g_Texture0,uv); color.r*=0.5;";
    let mut combine = pass("color=texture(g_Texture0,uv); color.r+=texture(g_Texture1,uv).r;");
    combine.binds.push((1, "previous".into()));
    let chain = vec![
        effect("tint", vec![pass(half)]),
        effect("multipass", vec![pass(half), pass(identity), combine]),
    ];
    let progs = programs(&r, &chain);
    let mut targets = EffectTargets::default();
    for _ in 0..3 {
        let out = targets.run(&r, input, &chain, &progs, &[], 0.0, [0.5; 2], |_| None);
        assert_eq!(out.applied, ["tint", "multipass"]);
        near(&read(&r, out.image)[..4], &[75, 40, 20, 255]);
    }
    // Named downsample, then reading and writing the same named buffer, then
    // combining with the current effect's original input at full resolution.
    let mut down = pass(half);
    down.target = Some("small".into());
    down.binds.push((0, "previous".into()));
    let mut repeat = pass(half);
    repeat.target = Some("small".into());
    repeat.binds.push((0, "small".into()));
    let mut up = pass("color=texture(g_Texture0,uv); color.r+=texture(g_Texture1,uv).r;");
    up.binds = vec![(0, "small".into()), (1, "previous".into())];
    let mut named = effect("named", vec![down, repeat, up]);
    named.fbos = vec![("small".into(), 2.0)];
    let chain = vec![effect("tint", vec![pass(half)]), named];
    let progs = programs(&r, &chain);
    let out = targets.run(&r, input, &chain, &progs, &[], 0.0, [0.5; 2], |_| None);
    near(&read(&r, out.image)[..4], &[63, 40, 20, 255]);
    // A failed intermediate shader skips the whole effect, preserving order
    // and input for the next effect rather than sampling uninitialized FBOs.
    let chain = vec![
        effect("failed", vec![pass(half), pass(identity)]),
        effect("next", vec![pass(half)]),
    ];
    let mut progs = programs(&r, &chain);
    progs[1] = None;
    let out = targets.run(&r, input, &chain, &progs, &[], 0.0, [0.5; 2], |_| None);
    assert_eq!(out.applied, ["next"]);
    near(&read(&r, out.image)[..4], &[50, 40, 20, 255]);

    // The image occupies half its storage width; the mask occupies half
    // its storage height. Both must cover the entire visible layer.
    let mut pixels = vec![0u8; 8 * 8 * 4];
    let mut mask_pixels = vec![0u8; 8 * 8 * 4];
    for y in 0..8 {
        for x in 0..8 {
            let at = (y * 8 + x) * 4;
            if x < 4 {
                pixels[at..at + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
            if y < 4 {
                mask_pixels[at..at + 4].copy_from_slice(&[128, 128, 128, 255]);
            }
        }
    }
    let padded = EffectTexture {
        tex: r.upload_rgba(&pixels, 8, 8),
        w: 8,
        h: 8,
        uv_scale: (0.5, 1.0),
    };
    let mask = EffectTexture {
        tex: r.upload_rgba(&mask_pixels, 8, 8),
        w: 8,
        h: 8,
        uv_scale: (1.0, 0.5),
    };
    let mut masked=pass("color=texture(g_Texture0,uv); color.a*=texture(g_Texture1,uv*g_Texture1Resolution.zw/g_Texture1Resolution.xy).r;");
    masked.frag = masked.frag.replace(
        "out vec4 color;",
        "uniform vec4 g_Texture1Resolution; out vec4 color;",
    );
    masked.textures.insert(1, "mask".into());
    let chain = vec![effect("opacity", vec![masked])];
    let progs = programs(&r, &chain);
    let out = targets.run(&r, padded, &chain, &progs, &[], 0.0, [0.5; 2], |n| {
        (n == "mask").then_some(mask)
    });
    assert_eq!((out.image.w, out.image.h), (4, 8));
    let image = read(&r, out.image);
    for y in 1..7 {
        for x in 0..4 {
            near(
                &image[(y * 4 + x) * 4..(y * 4 + x + 1) * 4],
                &[255, 0, 0, 128],
            );
        }
    }

    // Shader translation must keep alternate local constants inside their
    // #if branches (stock godrays declares sampleCount in each branch).
    let assets = wallengine_we::AssetResolver::new(std::env::temp_dir());
    let source = r#"
uniform sampler2D g_Texture0;
varying vec2 uv;
void main() {
#if QUALITY == 0
 const int sampleCount = 30;
#else
 const int sampleCount = 50;
#endif
 gl_FragColor = texture(g_Texture0,uv) * (float(sampleCount) / 50.0);
}
"#;
    for quality in [0, 1] {
        let translated = wallengine_we::glsl::preprocess_we_glsl(
            source,
            wallengine_we::glsl::ShaderStage::Fragment,
            "synthetic",
            &assets,
            &HashMap::from([("QUALITY".into(), quality)]),
        );
        r.compile_effect(VERT, &translated)
            .expect("conditional local declarations keep scope");
    }

    // Puppet output must be independent of a prior quad's UV offset, color
    // key, and blend mode, and must honor its own opacity.
    let red = r.upload_rgba(&[255, 0, 0, 255].repeat(4), 2, 2);
    let key = wallengine_we::ColorkeyParams {
        color: [1.0, 0.0, 0.0],
        alpha: 0.0,
        fuzziness: 0.0,
        tolerance: 0.1,
    };
    r.begin_frame(8, 8, [0.0, 0.0, 0.0, 1.0]);
    r.draw_ortho_image(
        8,
        8,
        8.0,
        8.0,
        red,
        [0.0, 0.0],
        [8.0, 8.0],
        0.0,
        1.0,
        (1.0, 1.0),
        (0.7, 0.7),
        Some(key),
        9,
    );
    let tris = [
        [-4.0, 4.0, 0.0, 0.0],
        [4.0, 4.0, 1.0, 0.0],
        [-4.0, -4.0, 0.0, 1.0],
        [-4.0, -4.0, 0.0, 1.0],
        [4.0, 4.0, 1.0, 0.0],
        [4.0, -4.0, 1.0, 1.0],
    ];
    r.draw_puppet_tris(
        8,
        8,
        8.0,
        8.0,
        red,
        &tris,
        (1.0, 1.0),
        (0.0, 0.0),
        0.5,
        None,
        0,
    );
    let mut pixel = [0; 4];
    unsafe {
        r.gl.read_pixels(
            4,
            4,
            1,
            1,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixel)),
        );
    }
    near(&pixel[..3], &[128, 0, 0]);
    targets.clear(&r);
}
