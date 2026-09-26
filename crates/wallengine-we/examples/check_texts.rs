fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    let id = std::env::args().nth(1).unwrap_or_else(|| "3448877775".into());
    let dir = wallengine_we::workshop_dir().join(&id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    rt.tick(0.1);
    println!("texts={}", rt.texts.len());
    for t in &rt.texts {
        println!("  «{}» kind={:?} str={:?} pt={} origin={:?} rgba={}", t.name, t.kind, t.current, t.pointsize, t.origin, t.rgba.as_ref().map(|r|(r.width,r.height)).map(|x|format!("{x:?}")).unwrap_or("none".into()));
    }
}
