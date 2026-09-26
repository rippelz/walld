fn main() {
    let id = "1932433918";
    let dir = wallengine_we::workshop_dir().join(id);
    let rt = wallengine_we::WeSceneRuntime::load(&dir, id, "rdr2").unwrap();
    for (i, layer) in rt.images.iter().enumerate() {
        if let Some(ref t) = layer.rgba {
            let path = format!("/tmp/layer{i}.ppm");
            let mut f = std::fs::File::create(&path).unwrap();
            use std::io::Write;
            writeln!(f, "P6\n{} {}\n255", t.width, t.height).unwrap();
            for y in 0..t.height {
                for x in 0..t.width {
                    let idx = ((y * t.width + x) * 4) as usize;
                    f.write_all(&[t.rgba[idx], t.rgba[idx + 1], t.rgba[idx + 2]]).unwrap();
                }
            }
            println!("wrote {path} {}x{}", t.width, t.height);
        }
        if let Some(ref t) = layer.mask_rgba {
            let path = format!("/tmp/mask{i}_content.ppm");
            let mut f = std::fs::File::create(&path).unwrap();
            use std::io::Write;
            writeln!(f, "P6\n{} {}\n255", t.content_width, t.content_height).unwrap();
            let mut minx = u32::MAX;
            let mut miny = u32::MAX;
            let mut maxx = 0u32;
            let mut maxy = 0u32;
            for y in 0..t.content_height {
                for x in 0..t.content_width {
                    let idx = ((y * t.width + x) * 4) as usize;
                    let v = t.rgba[idx];
                    f.write_all(&[v, v, v]).unwrap();
                    if v > 32 {
                        minx = minx.min(x);
                        miny = miny.min(y);
                        maxx = maxx.max(x);
                        maxy = maxy.max(y);
                    }
                }
            }
            println!(
                "wrote {path} content {}x{} bright bbox ({},{})-({},{}) uv ({:.3},{:.3})-({:.3},{:.3})",
                t.content_width,
                t.content_height,
                minx,
                miny,
                maxx,
                maxy,
                minx as f32 / t.content_width as f32,
                miny as f32 / t.content_height as f32,
                maxx as f32 / t.content_width as f32,
                maxy as f32 / t.content_height as f32
            );
        }
    }
}
