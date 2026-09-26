//! Dump decoded .tex stats + PNG for eyeballing.
fn main() {
    let path = std::env::args().nth(1).expect("tex path");
    let out = std::env::args().nth(2).unwrap_or_else(|| "/tmp/tex_dump.png".into());
    let data = std::fs::read(&path).expect("read");
    let t = wallengine_we::decode_tex(&data).expect("decode");
    println!(
        "w={} h={} content={}x{} tex={}x{} fmt={:?}",
        t.width, t.height, t.content_width, t.content_height, t.texture_width, t.texture_height, t.format
    );
    // alpha stats
    let (mut zero, mut full, mut mid) = (0u32, 0u32, 0u32);
    for px in t.rgba.chunks(4) {
        match px[3] {
            0 => zero += 1,
            255 => full += 1,
            _ => mid += 1,
        }
    }
    let n = t.width * t.height;
    println!("alpha: zero={zero} full={full} mid={mid} total={n}");
    // first opaque row / last opaque row
    let (mut first, mut last) = (None, None);
    for (y, row) in t.rgba.chunks((t.width * 4) as usize).enumerate() {
        if row.chunks(4).any(|p| p[3] > 8) {
            if first.is_none() { first = Some(y); }
            last = Some(y);
        }
    }
    println!("opaque rows: first={first:?} last={last:?}");
    // write ppm -> png
    let (w, h) = (t.width as usize, t.height as usize);
    let ppm = format!("{out}.ppm");
    use std::io::Write;
    let mut f = std::fs::File::create(&ppm).unwrap();
    writeln!(f, "P6\n{w} {h}\n255").unwrap();
    for px in t.rgba.chunks(4) {
        f.write_all(&[px[0], px[1], px[2]]).unwrap();
    }
    let _ = std::process::Command::new("ffmpeg").args(["-y", "-loglevel", "error", "-i", &ppm, &out]).output();
    println!("wrote {out}");
}
