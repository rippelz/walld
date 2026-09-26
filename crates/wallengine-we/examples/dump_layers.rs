fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "3448877775".into());
    let dir = wallengine_we::workshop_dir().join(&id);
    let rt = match wallengine_we::WeSceneRuntime::load(&dir, &id, &id) {
        Ok(r) => r,
        Err(e) => { eprintln!("load err: {e}"); return; }
    };
    let ow = rt.ortho_width;
    let oh = rt.ortho_height;
    println!("ortho {ow}x{oh} images={} particles={}", rt.images.len(), rt.particles.len());
    for (i, d) in rt.image_draws().into_iter().enumerate() {
        let layer = &rt.images[d.layer_index];
        let [sx, sy] = wallengine_we::camera_to_screen(d.origin[0], d.origin[1], ow, oh);
        let hw = d.size[0] * 0.5;
        let hh = d.size[1] * 0.5;
        let name: String = layer.name.chars().take(32).collect();
        println!(
            "[{i:3}] «{name}» origin_s=({sx:.0},{sy:.0}) size=({:.0}x{:.0}) AABB=[{:.0}..{:.0},{:.0}..{:.0}] ang={:.3}",
            d.size[0], d.size[1], sx - hw, sx + hw, sy - hh, sy + hh, d.angle_z
        );
    }
}
