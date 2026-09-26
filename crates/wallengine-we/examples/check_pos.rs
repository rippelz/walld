fn main() {
    let id = "1932433918";
    let dir = wallengine_we::workshop_dir().join(id);
    let rt = wallengine_we::WeSceneRuntime::load(&dir, id, "rdr2").unwrap();
    for (i, L) in rt.images.iter().enumerate() {
        if let Some(ref t) = L.rgba {
            let uv = t.content_uv_scale();
            println!("layer[{i}] albedo buf={}x{} content={}x{} uv_scale=({:.3},{:.3})",
                t.width, t.height, t.content_width, t.content_height, uv.0, uv.1);
        }
        if let Some(ref t) = L.mask_rgba {
            let uv = t.content_uv_scale();
            println!("layer[{i}] MASK  buf={}x{} content={}x{} uv_scale=({:.3},{:.3})",
                t.width, t.height, t.content_width, t.content_height, uv.0, uv.1);
            // bright centroid in content UV
            let mut bright=0u64; let mut cx=0u64; let mut cy=0u64;
            for y in 0..t.content_height {
                for x in 0..t.content_width {
                    let i=((y*t.width+x)*4) as usize;
                    if t.rgba[i] > 32 { bright+=1; cx+=x as u64; cy+=y as u64; }
                }
            }
            if bright>0 {
                println!("  content bright centroid uv=({:.3},{:.3}) n={bright}",
                    cx as f64/bright as f64/t.content_width as f64,
                    cy as f64/bright as f64/t.content_height as f64);
            }
        }
    }
    println!("particles preticked: first sys count={}", rt.particles.first().map(|p| p.particles.len()).unwrap_or(0));
    for (i,p) in rt.particles.iter().enumerate() {
        println!("  part[{i}] {} alive={}", p.name, p.alive().count());
    }
}
