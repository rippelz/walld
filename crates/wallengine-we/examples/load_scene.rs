fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let ids: Vec<String> = std::env::args().skip(1).collect();
    let ids = if ids.is_empty() {
        vec![
            "3327773109".into(),
            "1932433918".into(),
            "3555083647".into(),
        ]
    } else {
        ids
    };
    for id in ids {
        let dir = wallengine_we::workshop_dir().join(&id);
        println!("=== loading {id} from {} ===", dir.display());
        match wallengine_we::WeSceneRuntime::load(&dir, &id, &id) {
            Ok(rt) => {
                println!(
                    "  ok «{}» ortho={}x{} images={} particles={} animated={}",
                    rt.title,
                    rt.ortho_width,
                    rt.ortho_height,
                    rt.images.len(),
                    rt.particles.len(),
                    rt.is_animated()
                );
                for (i, img) in rt.images.iter().enumerate() {
                    println!(
                        "    img[{i}] «{}» size={:?} scale={:?} effects={} mask={} phase={}",
                        img.name,
                        img.size,
                        img.scale,
                        img.effects.len(),
                        img.mask_rgba.is_some(),
                        img.phase_rgba.is_some()
                    );
                    for e in &img.effects {
                        println!(
                            "      effect {:?} strength={} speed={} mask={:?}",
                            e.kind, e.strength, e.speed, e.mask_tex
                        );
                    }
                }
                for (i, p) in rt.particles.iter().enumerate() {
                    println!(
                        "    part[{i}] «{}» count={} origin={:?}",
                        p.name,
                        p.particles.len(),
                        p.origin_cam
                    );
                }
            }
            Err(e) => println!("  ERR: {e}"),
        }
    }
}
