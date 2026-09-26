fn main() {
    let id = std::env::args().nth(1).unwrap();
    let dir = wallengine_we::workshop_dir().join(&id);
    let rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    for l in &rt.images {
        for e in &l.effect_passes {
            if e.file.contains(&std::env::args().nth(2).unwrap_or("shake".into())) {
                std::fs::write("/tmp/pass.frag", &e.passes[0].frag).unwrap();
                std::fs::write("/tmp/pass.vert", &e.passes[0].vert).unwrap();
                println!("wrote {}", e.file);
                return;
            }
        }
    }
}
