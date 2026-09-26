fn main() {
    let id = "1932433918";
    let dir = wallengine_we::workshop_dir().join(id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, id, id).unwrap();
    for _ in 0..60 { rt.tick(1.0/30.0); }
    for sys in &rt.particles {
        if !sys.name.to_ascii_lowercase().contains("snow") && !sys.name.to_ascii_lowercase().contains("ember") && !sys.name.to_ascii_lowercase().contains("smoke") {
            continue;
        }
        println!("=== {} blend={} ob={:.2} tex={} ===", sys.name, sys.blending, sys.overbright, sys.texture.is_some());
        let mut n=0;
        for p in sys.alive() {
            if n < 5 {
                println!("  color=({:.3},{:.3},{:.3}) alpha={:.3} size={:.1}", p.color[0], p.color[1], p.color[2], p.alpha, p.size);
            }
            n+=1;
        }
        println!("  alive={n}");
    }
}
