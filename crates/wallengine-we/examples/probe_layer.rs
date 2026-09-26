fn main() {
    env_logger::init();
    let id = std::env::args().nth(1).unwrap();
    let want = std::env::args().nth(2).unwrap_or_default();
    let dir = wallengine_we::workshop_dir().join(&id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    let ow = rt.ortho_width; let oh = rt.ortho_height;
    for (label, n) in [("t=0", 0), ("t=2s", 60)] {
        for _ in 0..n { rt.tick(1.0/30.0); }
        for l in rt.images.iter().filter(|l| l.name.contains(&want)) {
            let s = wallengine_we::camera_to_screen(l.origin[0], l.origin[1], ow, oh);
            println!("{label} «{}» screen=({:.0},{:.0}) scale={:?} vis={}", l.name, s[0], s[1], l.scale, l.visible);
        }
    }
}
