//! Debug: print image draw math for a scene at a few pixels.
fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "2263538027".into());
    let dir = wallengine_we::workshop_dir().join(&id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    for _ in 0..5 { rt.tick(1.0/30.0); }
    let ow = rt.ortho_width; let oh = rt.ortho_height;
    let (vw, vh) = (ow, oh);
    let (fit_s, _ox, _oy) = wallengine_we::cover_fit(ow, oh, vw, vh);
    for d in rt.image_draws() {
        let layer = &rt.images[d.layer_index];
        println!("layer «{}»", layer.name);
        println!("  layer.origin={:?} layer.size={:?} layer.scale={:?} angles={:?}", layer.origin, layer.size, layer.scale, layer.angles);
        println!("  layer.rgba={} puppet={} mask={}", layer.rgba.is_some(), layer.puppet.is_some(), layer.mask_rgba.is_some());
        if let Some(t) = &layer.rgba {
            println!("  tex w={} h={} content={}x{}", t.width, t.height, t.content_width, t.content_height);
            // sample a horizontal line of texels at v=0.5
            for (u, v) in [(0.5f32, 0.5f32), (0.5, 0.05), (0.5, 0.95), (0.1, 0.5), (0.9, 0.5)] {
                let x = (u * t.width as f32) as u32;
                let y = (v * t.height as f32) as u32;
                let i = ((y * t.width + x) * 4) as usize;
                println!("    u={u} v={v} -> rgba=({},{},{},{})", t.rgba[i], t.rgba[i+1], t.rgba[i+2], t.rgba[i+3]);
            }
        }
        println!("  d.origin={:?} d.size={:?} d.angle_z={} uv_scale={:?} uv_offset={:?}", d.origin, d.size, d.angle_z, d.uv_scale, d.uv_offset);
        // replicate harness math at center + a few points
        for (px, py) in [(vw*0.5, vh*0.5), (vw*0.25, vh*0.25), (vw*0.5, vh*0.05)] {
            let cam_x = (px + 0.5 - vw*0.5) / fit_s;
            let cam_y = (vh*0.5 - (py + 0.5)) / fit_s;
            let dx = cam_x - d.origin[0];
            let dy = cam_y - d.origin[1];
            let u = dx / d.size[0] + 0.5;
            let v = 0.5 - dy / d.size[1];
            println!("  px=({px:.0},{py:.0}) cam=({cam_x:.1},{cam_y:.1}) u={u:.3} v={v:.3}");
        }
    }
}
