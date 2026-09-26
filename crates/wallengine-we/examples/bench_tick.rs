fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "3448877775".into());
    let dir = wallengine_we::workshop_dir().join(&id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    rt.tick(1.0 / 30.0); // warm
    let t0 = std::time::Instant::now();
    for _ in 0..100 { rt.tick(1.0 / 30.0); }
    println!("avg tick: {:.2} ms", t0.elapsed().as_secs_f64() * 10.0);
}
