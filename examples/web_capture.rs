//! Exercise the real web frame source without changing desktop wallpapers.
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};
use walld::video::VideoDecoder;

fn main() -> Result<(), String> {
    env_logger::init();
    let args: Vec<String> = std::env::args().collect();
    let input = args
        .get(1)
        .ok_or("usage: web_capture <index.html> <output.png> [seconds]")?;
    let output = args.get(2).ok_or("missing output.png")?;
    let seconds = args.get(3).and_then(|v| v.parse().ok()).unwrap_or(8);
    let decoder = VideoDecoder::start_web(Path::new(input), 15, 960)?;
    let start = Instant::now();
    let mut frames = 0;
    let mut changes = 0;
    let mut previous = Vec::new();
    let mut last = None;
    while start.elapsed() < Duration::from_secs(seconds) {
        if let Some(frame) = decoder.try_frame() {
            if !previous.is_empty() && previous != frame.rgba {
                changes += 1;
            }
            previous = frame.rgba.clone();
            frames += 1;
            last = Some(frame);
        }
        thread::sleep(Duration::from_millis(10));
    }
    let frame = last.ok_or("browser produced no frames")?;
    image::save_buffer(
        output,
        &frame.rgba,
        frame.width,
        frame.height,
        image::ColorType::Rgba8,
    )
    .map_err(|e| e.to_string())?;
    println!(
        "frames={frames} changes={changes} size={}x{} elapsed={:.1}s output={output}",
        frame.width,
        frame.height,
        start.elapsed().as_secs_f64()
    );
    drop(decoder);
    Ok(())
}
