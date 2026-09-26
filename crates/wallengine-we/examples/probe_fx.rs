fn main() {
    env_logger::init();
    let id = std::env::args().nth(1).unwrap();
    let dir = wallengine_we::workshop_dir().join(&id);
    let rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    for l in &rt.images {
        if l.effect_passes.is_empty() { continue; }
        println!("«{}»", l.name);
        for e in &l.effect_passes {
            for p in &e.passes {
                let mut u: Vec<_> = p.uniforms.keys().cloned().collect();
                u.sort();
                println!("   {} passes=1 blend={} tex={:?} combos={:?}", e.file, p.blending, p.textures, p.combos);
                println!("      uniforms: {}", u.join(", "));
            }
        }
    }
}
