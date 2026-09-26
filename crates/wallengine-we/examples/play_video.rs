fn main() {
    let entries = wallengine_we::scan_all();
    println!("scanned {}", entries.len());
    let vid = entries.iter().find(|e| matches!(e.project.wallpaper_type, wallengine_we::WallpaperType::Video));
    let Some(e) = vid else { eprintln!("no video"); return; };
    println!("playing {} {}", e.id, e.project.title);
    let req = wallengine_we::PlayRequest {
        wallpaper_dir: e.dir.clone(),
        workshop_id: e.id.clone(),
        wallpaper_type: e.project.wallpaper_type,
        monitors: vec![],
        silent: true,
        fps: 30,
        backend: wallengine_we::PlayBackend::Walld,
    };
    match wallengine_we::play(&req) {
        Ok(s) => println!("ok {:?} {}", s.backend, s.detail),
        Err(e) => eprintln!("err {e}"),
    }
    std::thread::sleep(std::time::Duration::from_secs(4));
    wallengine_we::stop_all();
    println!("stopped");
}
