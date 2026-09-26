//! Headless render of a WE scene through walld's real GL `Renderer`
//! (EGL pbuffer, same shaders/draw calls as the live daemon).
//!
//! Usage: gl_render_scene <workshop-id> <out.png> [width height]

use glow::HasContext;
#[derive(Clone, Copy)]

struct Tex {
    tex: glow::Texture,
    w: u32,
    h: u32,
    uv_scale: (f32, f32),
}

fn main() {
    env_logger::init();
    let id = std::env::args().nth(1).unwrap_or_else(|| "3448877775".into());
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| format!("/tmp/we_{id}_gl.png"));
    let vw = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1920i32);
    let vh = std::env::args()
        .nth(4)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1080i32);

    let mut renderer = walld::render::Renderer::headless().expect("headless EGL");
    renderer.make_pbuffer(vw, vh).expect("pbuffer");
    let dir = wallengine_we::workshop_dir().join(&id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    // WALLD_SIM_SECS: simulate this many seconds of scene time before the shot.
    let sim_secs: f32 = std::env::var("WALLD_SIM_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.1);
    let steps = (sim_secs * 30.0).max(1.0) as u32;
    for _ in 0..steps {
        rt.tick(1.0 / 30.0);
    }

    // Upload layer / mask / phase textures exactly like walld main.rs.
    let mut layer_tex: Vec<Option<Tex>> = Vec::new();
    let mut mask_tex: Vec<Option<Tex>> = Vec::new();
    let mut phase_tex: Vec<Option<Tex>> = Vec::new();
    for layer in &rt.images {
        layer_tex.push(layer.rgba.as_ref().map(|t| Tex {
            tex: renderer.upload_rgba(&t.rgba, t.width, t.height),
            w: t.width,
            h: t.height,
            uv_scale: t.content_uv_scale(),
        }));
        mask_tex.push(layer.mask_rgba.as_ref().map(|t| Tex {
            tex: renderer.upload_rgba(&t.rgba, t.width, t.height),
            w: t.width,
            h: t.height,
            uv_scale: t.content_uv_scale(),
        }));
        phase_tex.push(layer.phase_rgba.as_ref().map(|t| Tex {
            tex: renderer.upload_rgba(&t.rgba, t.width, t.height),
            w: t.width,
            h: t.height,
            uv_scale: t.content_uv_scale(),
        }));
    }
    let particle_tex: Vec<Option<Tex>> = rt
        .particles
        .iter()
        .map(|sys| {
            sys.texture.as_ref().map(|t| Tex {
                tex: renderer.upload_rgba(&t.rgba, t.width, t.height),
                w: t.width,
                h: t.height,
                uv_scale: t.content_uv_scale(),
            })
        })
        .collect();
    let text_tex: Vec<Option<Tex>> = rt
        .texts
        .iter()
        .map(|txt| {
            txt.rgba.as_ref().map(|t| Tex {
                tex: renderer.upload_rgba(&t.rgba, t.width, t.height),
                w: t.width,
                h: t.height,
                uv_scale: (1.0, 1.0),
            })
        })
        .collect();

    // Compile effect passes per layer. Keep a slot per authoring pass so a
    // failed compile does not shift later programs onto the wrong effect.
    let mut fx_progs: std::collections::HashMap<usize, Vec<Option<glow::Program>>> =
        std::collections::HashMap::new();
    let mut fx_fail = 0usize;
    let mut fx_ok = 0usize;
    for (i, layer) in rt.images.iter().enumerate() {
        let mut progs = Vec::new();
        for eff in &layer.effect_passes {
            for p in &eff.passes {
                match renderer.compile_effect(&p.vert, &p.frag) {
                    Ok(pr) => {
                        progs.push(Some(pr));
                        fx_ok += 1;
                    }
                    Err(e) => {
                        progs.push(None);
                        fx_fail += 1;
                        log::warn!("effect {} compile failed: {}", eff.file, e);
                    }
                }
            }
        }
        if progs.iter().any(|p| p.is_some()) {
            fx_progs.insert(i, progs);
        }
    }
    eprintln!("effect passes: {fx_ok} compiled, {fx_fail} failed");

    // Effect sampler textures (flow maps / opacity masks), by asset name.
    let mut fx_tex: std::collections::HashMap<String, Tex> = std::collections::HashMap::new();
    for layer in &rt.images {
        for eff in &layer.effect_passes {
            for p in &eff.passes {
                for name in p.textures.values() {
                    if fx_tex.contains_key(name) {
                        continue;
                    }
                    if let Ok(t) = rt.assets.load_tex(name) {
                        fx_tex.insert(
                            name.clone(),
                            Tex {
                                tex: renderer.upload_rgba(&t.rgba, t.width, t.height),
                                w: t.width,
                                h: t.height,
                                uv_scale: t.content_uv_scale(),
                            },
                        );
                    }
                }
            }
        }
    }

    let ortho_w = rt.ortho_width;
    let ortho_h = rt.ortho_height;
    let time = rt.time();
    let draw_list = rt.scene_draw_list();
    let puppet_batches: Vec<(usize, Vec<[f32; 4]>)> = rt
        .images
        .iter()
        .enumerate()
        .filter_map(|(i, layer)| {
            layer.puppet.as_ref().map(|mesh| {
                (
                    i,
                    mesh.to_camera_tris_crop(
                        [layer.origin[0], layer.origin[1]],
                        [layer.scale[0], layer.scale[1]],
                        layer.angles[2],
                        layer.crop_offset,
                        layer.size,
                    ),
                )
            })
        })
        .collect();

    renderer.begin_frame(vw, vh, [rt.clear_color[0], rt.clear_color[1], rt.clear_color[2], 1.0]);

    let mut effect_targets = walld::effects::EffectTargets::default();
    for item in &draw_list {
        match item {
            wallengine_we::SceneDrawItem::Image(d) => {
                let mut d = d.clone();
                let Some(Some(albedo)) = layer_tex.get(d.layer_index) else {
                    continue;
                };
                if !wallengine_we::gl_blend_mode_supported(d.color_blend_mode) {
                    continue;
                }
                // Effect passes: render the layer's texture through each pass
                // (ping-pong FBOs), then draw the result in its place.
                let mut albedo = albedo.clone();
                if let Some(progs) = fx_progs.get(&d.layer_index) {
                    let output = effect_targets.run(
                        &renderer,
                        walld::effects::EffectTexture { tex: albedo.tex, w: albedo.w, h: albedo.h, uv_scale: albedo.uv_scale },
                        &rt.images[d.layer_index].effect_passes, progs, &rt.graph,
                        time, [0.5, 0.5],
                        |name| fx_tex.get(name).map(|t| walld::effects::EffectTexture { tex: t.tex, w: t.w, h: t.h, uv_scale: t.uv_scale }),
                    );
                    d.rebase_texture_uv(albedo.uv_scale, output.image.uv_scale);
                    albedo = Tex { tex: output.image.tex, w: output.image.w, h: output.image.h, uv_scale: output.image.uv_scale };
                    d.suppress_applied_effects(&output.applied);
                }
                let albedo = &albedo;
                if d.has_puppet {
                    if let Some((_, tris)) =
                        puppet_batches.iter().find(|(i, _)| *i == d.layer_index)
                    {
                        renderer.draw_puppet_tris(
                            vw, vh, ortho_w, ortho_h, albedo.tex, tris,
                            (d.uv_scale[0], d.uv_scale[1]),
                            (d.uv_offset[0], d.uv_offset[1]),
                                    d.alpha, d.colorkey, d.color_blend_mode,
                        );
                        continue;
                    }
                }
                let has_wf = d.has_waterflow
                    && mask_tex.get(d.layer_index).and_then(|t| t.as_ref()).is_some()
                    && phase_tex.get(d.layer_index).and_then(|t| t.as_ref()).is_some();
                let has_op = d.has_opacity
                    && mask_tex.get(d.layer_index).and_then(|t| t.as_ref()).is_some();
                if has_wf {
                    let mask = mask_tex[d.layer_index].as_ref().unwrap();
                    let phase = phase_tex[d.layer_index].as_ref().unwrap();
                    let wf = d.waterflow.as_ref().unwrap();
                    let (mu, mv) = mask.uv_scale;
                    renderer.draw_waterflow(
                        vw,
                        vh,
                        ortho_w,
                        ortho_h,
                        albedo.tex,
                        mask.tex,
                        phase.tex,
                        mask.w,
                        mask.h,
                        (mask.w as f32 * mu) as u32,
                        (mask.h as f32 * mv) as u32,
                        d.origin,
                        d.size,
                        d.angle_z,
                        time,
                        wf.speed,
                        wf.strength,
                        wf.phasescale,
                        wf.feather,
                        (d.uv_scale[0], d.uv_scale[1]),
                        (d.uv_offset[0], d.uv_offset[1]),
                    );
                } else if has_op {
                    let mask = mask_tex[d.layer_index].as_ref().unwrap();
                    let alpha = d.opacity.as_ref().map(|o| o.strength).unwrap_or(1.0) * d.alpha;
                    renderer.draw_opacity_masked(
                        vw,
                        vh,
                        ortho_w,
                        ortho_h,
                        albedo.tex,
                        mask.tex,
                        d.origin,
                        d.size,
                        d.angle_z,
                        alpha,
                        (d.uv_scale[0], d.uv_scale[1]),
                        (d.uv_offset[0], d.uv_offset[1]),
                        mask.uv_scale,
                    );
                } else {
                    renderer.draw_ortho_image(
                        vw,
                        vh,
                        ortho_w,
                        ortho_h,
                        albedo.tex,
                        d.origin,
                        d.size,
                        d.angle_z,
                        d.alpha,
                        (d.uv_scale[0], d.uv_scale[1]),
                        (d.uv_offset[0], d.uv_offset[1]),
                        d.colorkey,
                        d.color_blend_mode,
                    );
                }
            }
            wallengine_we::SceneDrawItem::Text(td) => {
                let Some(Some(tex)) = text_tex.get(td.text_index) else {
                    continue;
                };
                renderer.draw_ortho_image(
                    vw,
                    vh,
                    ortho_w,
                    ortho_h,
                    tex.tex,
                    td.origin,
                    td.size,
                    0.0,
                    td.alpha,
                    (1.0, 1.0),
                    (0.0, 0.0),
                    None,
                    0,
                );
            }
            wallengine_we::SceneDrawItem::Particle(pd) => {
                let Some(sys) = rt.particles.get(pd.system_index) else {
                    continue;
                };
                // Textured sprites when the material provides a texture.
                if let Some(Some(ptex)) = particle_tex.get(pd.system_index) {
                    let verts =
                        sys.sprite_vertices(ortho_w, ortho_h, vw as f32, vh as f32);
                    if !verts.is_empty() {
                        renderer.draw_particle_sprites(
                            vw,
                            vh,
                            ptex.tex,
                            &verts,
                            sys.is_additive(),
                            sys.overbright.max(0.0),
                        );
                    }
                    continue;
                }
                let mut data = Vec::new();
                let (fit_s, _, _) =
                    wallengine_we::cover_fit(ortho_w, ortho_h, vw as f32, vh as f32);
                // LWE size = full sprite width after sizerandom/2.
                let sc = sys.scale[0].abs().max(sys.scale[1].abs()).max(0.01);
                let ob = sys.overbright.max(0.0);
                let additive = sys.is_additive();
                for p in sys.alive() {
                    let cam = sys.local_to_camera(p.pos);
                    let uv = wallengine_we::camera_to_viewport_uv(
                        cam[0], cam[1], ortho_w, ortho_h, vw as f32, vh as f32,
                    );
                    let size_px = (p.size * sc * fit_s * 1.8).max(1.5);
                    let size_n = size_px / (vw as f32).min(vh as f32).max(1.0);
                    data.push(uv[0]);
                    data.push(uv[1]);
                    data.push(size_n.max(0.0015));
                    data.push(p.alpha);
                    data.push((p.color[0] * ob).min(4.0));
                    data.push((p.color[1] * ob).min(4.0));
                    data.push((p.color[2] * ob).min(4.0));
                }
                if !data.is_empty() {
                    renderer.draw_points_ex(vw, vh, &data, additive);
                }
            }
        }
    }

    effect_targets.clear(&renderer);

    // Read back (GL origin bottom-left) and flip to top-left PNG rows.
    let gl = &renderer.gl;
    let mut buf = vec![0u8; (vw * vh * 4) as usize];
    unsafe {
        gl.read_pixels(
            0,
            0,
            vw,
            vh,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut buf)),
        );
    }
    let row = vw as usize * 4;
    let mut rows: Vec<u8> = Vec::with_capacity(buf.len());
    for y in (0..vh as usize).rev() {
        rows.extend_from_slice(&buf[y * row..(y + 1) * row]);
    }
    let mut f = std::fs::File::create(format!("{out}.ppm")).unwrap();
    use std::io::Write;
    writeln!(f, "P6\n{vw} {vh}\n255").unwrap();
    for px in rows.chunks(4) {
        f.write_all(&px[..3]).unwrap();
    }
    let st = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-i", &format!("{out}.ppm"), &out])
        .status();
    eprintln!("wrote {out} ({st:?})");
}
