fn main() {
    let id = std::env::args().nth(1).unwrap();
    let dir = wallengine_we::workshop_dir().join(&id);
    let rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    println!("animated={} particles={} texts={}", rt.is_animated(), rt.particles.len(), rt.texts.len());
    for l in &rt.images {
        let frames = l.rgba.as_ref().map(|t| t.frames.len()).unwrap_or(0);
        let fx: Vec<String> = l.effects.iter().map(|e| format!("{:?}", e.kind)).collect();
        if l.visible || frames > 0 || !fx.is_empty() {
            println!("  «{}» vis={} frames={} puppet={} effects={:?}", l.name, l.visible, frames, l.puppet.is_some(), fx);
        }
    }
}
