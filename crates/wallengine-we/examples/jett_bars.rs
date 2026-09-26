fn main() {
    let id = "3164376283";
    let dir = wallengine_we::workshop_dir().join(id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, id, id).expect("load");
    println!("before={}", rt.images.len());
    for i in 0..15 {
        rt.tick(1.0 / 60.0);
        if i == 0 || i == 14 {
            println!("tick {i} images={}", rt.images.len());
        }
    }
    let mut n = 0;
    for l in &rt.images {
        if !l.name.starts_with("dyn:") { continue; }
        n += 1;
        if n <= 5 {
            println!(
                "«{}» vis={} origin_cam=({:.1},{:.1}) size=({:.2}x{:.2}) scale=({:.2},{:.2}) ang={:.3} alpha={:.2} color={:?}",
                l.name, l.visible, l.origin[0], l.origin[1],
                l.size[0], l.size[1], l.scale[0], l.scale[1], l.angles[2], l.alpha, l.script_color
            );
        }
    }
    println!("dyn_count={n}");
    let draws = rt.image_draws();
    let dyn_draws: Vec<_> = draws.iter().filter(|d| rt.images[d.layer_index].name.starts_with("dyn:")).collect();
    println!("dyn_draws={}", dyn_draws.len());
    for d in dyn_draws.iter().take(5) {
        println!("  draw size={:?} origin={:?} ang={:.3} alpha={:.2}", d.size, d.origin, d.angle_z, d.alpha);
    }
}
