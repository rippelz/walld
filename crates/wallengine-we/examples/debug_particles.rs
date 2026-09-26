fn main() {
    let id = "1932433918";
    let dir = wallengine_we::workshop_dir().join(id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, id, "rdr2").unwrap();
    let ow = rt.ortho_width;
    let oh = rt.ortho_height;
    println!("ortho {ow}x{oh}");

    // One tick only so we see fresh spawns
    rt.tick(0.016);

    for (si, sys) in rt.particles.iter().enumerate() {
        let [osx, osy] = wallengine_we::camera_to_screen(sys.origin_cam[0], sys.origin_cam[1], ow, oh);
        println!("\n=== [{si}] {} ===", sys.name);
        println!("  origin_cam=({:.1},{:.1}) origin_screen=({:.1},{:.1})",
            sys.origin_cam[0], sys.origin_cam[1], osx, osy);
        println!("  scale=({:.3},{:.3}) angle_z={:.3}", sys.scale[0], sys.scale[1], sys.angle_z);

        let mut n = 0;
        let mut min_sx = f32::MAX;
        let mut max_sx = f32::MIN;
        let mut min_sy = f32::MAX;
        let mut max_sy = f32::MIN;
        let mut samples = Vec::new();
        for p in sys.alive() {
            let cam = sys.local_to_camera(p.pos);
            let [sx, sy] = wallengine_we::camera_to_screen(cam[0], cam[1], ow, oh);
            min_sx = min_sx.min(sx); max_sx = max_sx.max(sx);
            min_sy = min_sy.min(sy); max_sy = max_sy.max(sy);
            if samples.len() < 3 {
                samples.push((p.pos, cam, (sx, sy), p.vel));
            }
            n += 1;
        }
        println!("  alive={n} screen AABB x=[{min_sx:.0}..{max_sx:.0}] y=[{min_sy:.0}..{max_sy:.0}]");
        for (i, (local, cam, (sx,sy), vel)) in samples.iter().enumerate() {
            println!("  p{i} local=({:.0},{:.0}) cam=({:.0},{:.0}) screen=({:.0},{:.0}) vel=({:.1},{:.1})",
                local[0], local[1], cam[0], cam[1], sx, sy, vel[0], vel[1]);
        }
    }

    // Opacity layer
    println!("\n=== images/opacity ===");
    for d in rt.image_draws() {
        let L = &rt.images[d.layer_index];
        println!("  [{}] op={} mask={} size=({:.0}x{:.0}) origin_cam=({:.0},{:.0})",
            d.layer_index, d.has_opacity, L.mask_rgba.is_some(), d.size[0], d.size[1], d.origin[0], d.origin[1]);
        if let Some(ref m) = L.mask_rgba {
            // Sample mask center vs corners brightness
            let w = m.width as usize;
            let h = m.height as usize;
            let sample = |x: usize, y: usize| -> u8 {
                let i = (y * w + x) * 4;
                m.rgba[i] // R
            };
            let pts = [
                ("TL", 0, 0),
                ("TR", w-1, 0),
                ("BL", 0, h-1),
                ("BR", w-1, h-1),
                ("C", w/2, h/2),
                ("moon?", w*3/4, h/4),
            ];
            print!("  mask {}x{} samples:", w, h);
            for (name, x, y) in pts {
                print!(" {name}={}", sample(x, y));
            }
            println!();
        }
    }
}
