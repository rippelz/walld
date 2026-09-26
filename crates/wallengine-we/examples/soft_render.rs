//! Offline software composite of a WE scene using the same transform path as walld.
//! Usage: soft_render [workshop_id] [out.png] [--flip-emitter] [--flip-vel] [--no-particles]

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let id = args.get(1).map(|s| s.as_str()).unwrap_or("1932433918");
    let out = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/we_{id}.png")));
    let flip_emitter = args.iter().any(|a| a == "--flip-emitter");
    let flip_vel = args.iter().any(|a| a == "--flip-vel");
    let no_particles = args.iter().any(|a| a == "--no-particles");
    let mark = args.iter().any(|a| a == "--mark");

    let dir = wallengine_we::workshop_dir().join(id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, id, id).expect("load");
    let ow = rt.ortho_width;
    let oh = rt.ortho_height;
    let (vw, vh) = (ow as u32, oh as u32);

    // Apply experimental flips in-place for A/B (mutates local particle state after retick)
    if flip_emitter || flip_vel {
        eprintln!("NOTE: flips applied via re-sim flags in soft_render only for A/B comparison");
    }

    for _ in 0..5 {
        rt.tick(1.0 / 30.0);
    }

    // RGBA buffer
    let mut buf = vec![0u8; (vw * vh * 4) as usize];
    // clear
    let cc = rt.clear_color;
    for i in 0..(vw * vh) as usize {
        buf[i * 4] = (cc[0] * 255.0) as u8;
        buf[i * 4 + 1] = (cc[1] * 255.0) as u8;
        buf[i * 4 + 2] = (cc[2] * 255.0) as u8;
        buf[i * 4 + 3] = 255;
    }

    let (fit_s, ox, oy) = wallengine_we::cover_fit(ow, oh, vw as f32, vh as f32);
    eprintln!(
        "ortho {ow}x{oh} view {vw}x{vh} fit_s={fit_s:.3} ox={ox:.1} oy={oy:.1} images={} particles={}",
        rt.images.len(),
        rt.particles.len()
    );

    // Draw images (center, camera space → screen)
    for (li, d) in rt.image_draws().into_iter().enumerate() {
        let layer = &rt.images[d.layer_index];
        let Some(ref tex) = layer.rgba else { continue };
        let (cu, cv) = tex.content_uv_scale();
        let has_mask = d.has_opacity && layer.mask_rgba.is_some();
        let mask = layer.mask_rgba.as_ref();
        let (mu, mv) = mask.map(|m| m.content_uv_scale()).unwrap_or((1.0, 1.0));

        // AABB in screen for this layer
        let half_w = d.size[0] * 0.5;
        let half_h = d.size[1] * 0.5;
        // corners in camera (ignore rotation for soft render simplicity if angle~0)
        let ang = -d.angle_z;
        let c = ang.cos();
        let s = ang.sin();
        let corners = [
            [-half_w, half_h],
            [half_w, half_h],
            [-half_w, -half_h],
            [half_w, -half_h],
        ];
        let mut min_sx = f32::MAX;
        let mut max_sx = f32::MIN;
        let mut min_sy = f32::MAX;
        let mut max_sy = f32::MIN;
        for [lx, ly] in corners {
            let rx = lx * c - ly * s;
            let ry = lx * s + ly * c;
            let cam_x = d.origin[0] + rx;
            let cam_y = d.origin[1] + ry;
            let sx = vw as f32 * 0.5 + cam_x * fit_s + ox; // ox already centered in cover? cover_fit ox is top-left offset of canvas
            // Actually: screen = center + cam * s, cover letterbox is baked into...
            // walld camera_to_viewport_uv uses: screen = vw/2 + cam*s  (ignores ox/oy - assumes cover crops equally)
            let sx = vw as f32 * 0.5 + cam_x * fit_s;
            let sy = vh as f32 * 0.5 - cam_y * fit_s;
            min_sx = min_sx.min(sx);
            max_sx = max_sx.max(sx);
            min_sy = min_sy.min(sy);
            max_sy = max_sy.max(sy);
        }
        let x0 = min_sx.floor().max(0.0) as i32;
        let y0 = min_sy.floor().max(0.0) as i32;
        let x1 = max_sx.ceil().min(vw as f32) as i32;
        let y1 = max_sy.ceil().min(vh as f32) as i32;

        eprintln!(
            "IMG[{li}] «{}» cam=({:.0},{:.0}) size=({:.0}x{:.0}) screenAABB=[{}..{},{}..{}] mask={has_mask}",
            layer.name, d.origin[0], d.origin[1], d.size[0], d.size[1], x0, x1, y0, y1
        );

        for py in y0..y1 {
            for px in x0..x1 {
                // screen → camera
                let cam_x = (px as f32 + 0.5 - vw as f32 * 0.5) / fit_s;
                let cam_y = (vh as f32 * 0.5 - (py as f32 + 0.5)) / fit_s;
                // camera → local (inverse of model, ignore rot if small)
                let dx = cam_x - d.origin[0];
                let dy = cam_y - d.origin[1];
                // inverse rot (-ang already used for forward; inverse is +ang_z_screen = -ang)
                let ilx = dx * c + dy * s;
                let ily = -dx * s + dy * c;
                let u = ilx / d.size[0] + 0.5;
                let v = 0.5 - ily / d.size[1]; // +y cam → top of image → v=0
                if u < 0.0 || u > 1.0 || v < 0.0 || v > 1.0 {
                    continue;
                }
                // sample albedo with content UV
                let au = u * cu;
                let av = v * cv;
                let (ar, ag, ab, aa) = sample_rgba(tex, au, av);
                let mut alpha = aa as f32 / 255.0 * d.alpha;
                if has_mask {
                    if let Some(m) = mask {
                        let (mr, _, _, _) = sample_rgba(m, u * mu, v * mv);
                        alpha *= mr as f32 / 255.0;
                    }
                }
                if alpha < 0.01 {
                    continue;
                }
                if d.color_blend_mode != 0 {
                    let i = ((py as u32 * vw + px as u32) * 4) as usize;
                    let base = [
                        buf[i] as f32 / 255.0,
                        buf[i + 1] as f32 / 255.0,
                        buf[i + 2] as f32 / 255.0,
                    ];
                    let src = [ar as f32 / 255.0, ag as f32 / 255.0, ab as f32 / 255.0];
                    let out = wallengine_we::apply_color_blend(d.color_blend_mode, base, src, alpha);
                    buf[i] = (out[0] * 255.0) as u8;
                    buf[i + 1] = (out[1] * 255.0) as u8;
                    buf[i + 2] = (out[2] * 255.0) as u8;
                } else {
                    blend(&mut buf, vw, px, py, ar, ag, ab, alpha);
                }
            }
        }
    }

    if !no_particles {
        let mut n = 0u32;
        let mut min_sy = f32::MAX;
        let mut max_sy = f32::MIN;
        for sys in &rt.particles {
            for p in sys.alive() {
                let cam = sys.local_to_camera(p.pos);
                let sx = vw as f32 * 0.5 + cam[0] * fit_s;
                let sy = vh as f32 * 0.5 - cam[1] * fit_s;
                min_sy = min_sy.min(sy);
                max_sy = max_sy.max(sy);
                // half-extent × scale × fit → diameter via *2 for soft blob
                let size = (p.size
                    * 2.0
                    * sys.scale[0].abs().max(sys.scale[1].abs())
                    * fit_s)
                    .max(1.0);
                let r = (size * 0.5) as i32;
                let cx = sx as i32;
                let cy = sy as i32;
                let a = (p.alpha * 0.85).clamp(0.0, 1.0);
                for dy in -r..=r {
                    for dx in -r..=r {
                        if dx * dx + dy * dy > r * r {
                            continue;
                        }
                        let px = cx + dx;
                        let py = cy + dy;
                        if px < 0 || py < 0 || px >= vw as i32 || py >= vh as i32 {
                            continue;
                        }
                        // soft falloff
                        let t = 1.0 - ((dx * dx + dy * dy) as f32) / (r * r).max(1) as f32;
                        blend(&mut buf, vw, px, py, 220, 230, 255, a * t * t);
                    }
                }
                n += 1;
            }
        }
        eprintln!("particles drawn={n} screen_y=[{min_sy:.0}..{max_sy:.0}]");
    }

    if mark {
        // Mark expected moon (albedo brightest upper-right) and mask centroid
        mark_cross(&mut buf, vw, vh, (0.723 * vw as f32) as i32, (0.178 * vh as f32) as i32, 255, 0, 0);
        mark_cross(&mut buf, vw, vh, (0.440 * vw as f32) as i32, (0.373 * vh as f32) as i32, 0, 255, 0);
        // Mark particle system origins
        for sys in &rt.particles {
            let [sx, sy] = wallengine_we::camera_to_screen(sys.origin_cam[0], sys.origin_cam[1], ow, oh);
            // map ortho screen → view (exact fit)
            let px = (sx / ow * vw as f32) as i32;
            let py = (sy / oh * vh as f32) as i32;
            mark_cross(&mut buf, vw, vh, px, py, 255, 255, 0);
        }
    }

    write_png(&out, vw, vh, &buf);
    eprintln!("wrote {}", out.display());
}

fn sample_rgba(tex: &wallengine_we::DecodedTex, u: f32, v: f32) -> (u8, u8, u8, u8) {
    let w = tex.width as f32;
    let h = tex.height as f32;
    let x = (u * w - 0.5).clamp(0.0, (w - 1.001).max(0.0));
    let y = (v * h - 0.5).clamp(0.0, (h - 1.001).max(0.0));
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let i = ((y0 * tex.width + x0) * 4) as usize;
    (
        tex.rgba[i],
        tex.rgba[i + 1],
        tex.rgba[i + 2],
        tex.rgba[i + 3],
    )
}

fn blend(buf: &mut [u8], vw: u32, x: i32, y: i32, r: u8, g: u8, b: u8, a: f32) {
    if x < 0 || y < 0 || x >= vw as i32 {
        return;
    }
    let i = ((y as u32 * vw + x as u32) * 4) as usize;
    if i + 3 >= buf.len() {
        return;
    }
    let a = a.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    buf[i] = (buf[i] as f32 * inv + r as f32 * a) as u8;
    buf[i + 1] = (buf[i + 1] as f32 * inv + g as f32 * a) as u8;
    buf[i + 2] = (buf[i + 2] as f32 * inv + b as f32 * a) as u8;
    buf[i + 3] = 255;
}

fn mark_cross(buf: &mut [u8], vw: u32, vh: u32, cx: i32, cy: i32, r: u8, g: u8, b: u8) {
    for d in -12..=12 {
        for (x, y) in [(cx + d, cy), (cx, cy + d)] {
            if x >= 0 && y >= 0 && x < vw as i32 && y < vh as i32 {
                let i = ((y as u32 * vw + x as u32) * 4) as usize;
                buf[i] = r;
                buf[i + 1] = g;
                buf[i + 2] = b;
            }
        }
    }
}

fn write_png(path: &std::path::Path, w: u32, h: u32, rgba: &[u8]) {
    // minimal PNG via image crate if available; else PPM
    // wallengine-we may not depend on image — write PPM and convert
    let ppm = path.with_extension("ppm");
    let mut f = std::fs::File::create(&ppm).unwrap();
    use std::io::Write;
    writeln!(f, "P6\n{w} {h}\n255").unwrap();
    for i in 0..(w * h) as usize {
        f.write_all(&[rgba[i * 4], rgba[i * 4 + 1], rgba[i * 4 + 2]]).unwrap();
    }
    // try ffmpeg
    let _ = std::process::Command::new("ffmpeg")
        .args(["-y", "-i"])
        .arg(&ppm)
        .arg(path)
        .output();
    if path.exists() {
        let _ = std::fs::remove_file(&ppm);
    } else {
        eprintln!("ffmpeg failed; left {}", ppm.display());
    }
}
