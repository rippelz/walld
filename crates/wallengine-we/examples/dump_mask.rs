fn main() {
    let p = "/tmp/we-inspect/1932433918/materials/masks/opacity_mask_3005000a9a4412f3bdd7d02ff4adcd338116f5d2.tex";
    let data = std::fs::read(p).unwrap();
    let t = wallengine_we::decode_tex(&data).expect("decode");
    println!("{}x{} format={:?} flags={} free={:?}", t.width, t.height, t.format, t.flags, t.free_image);
    let mut rmin=255u8; let mut rmax=0u8; let mut rsum=0u64; let mut n=0u64;
    let mut bright=0u64; let mut cx=0u64; let mut cy=0u64;
    let mut gmin=255u8; let mut gmax=0u8; let mut amin=255u8; let mut amax=0u8;
    for y in 0..t.height {
        for x in 0..t.width {
            let i = ((y*t.width+x)*4) as usize;
            let (r,g,b,a)=(t.rgba[i], t.rgba[i+1], t.rgba[i+2], t.rgba[i+3]);
            rmin=rmin.min(r); rmax=rmax.max(r); rsum+=r as u64; n+=1;
            gmin=gmin.min(g); gmax=gmax.max(g);
            amin=amin.min(a); amax=amax.max(a);
            let v = r.max(g).max(a);
            if v>32 { bright+=1; cx+=x as u64; cy+=y as u64; }
        }
    }
    println!("R {}..{} mean={:.1}", rmin, rmax, rsum as f64/n as f64);
    println!("G {}..{}  A {}..{}", gmin, gmax, amin, amax);
    println!("bright(>32)={}", bright);
    if bright>0 {
        println!("bright centroid uv=({:.3},{:.3})",
            cx as f64/bright as f64 / t.width as f64,
            cy as f64/bright as f64 / t.height as f64);
    }
    // write raw preview
    image::save_buffer("/tmp/mask_out.png", &t.rgba, t.width, t.height, image::ColorType::Rgba8).ok();
    println!("wrote /tmp/mask_out.png");
}
