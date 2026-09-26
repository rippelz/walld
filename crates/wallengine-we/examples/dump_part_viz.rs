fn main() {
    let id = "1932433918";
    let dir = wallengine_we::workshop_dir().join(id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, id, "rdr2").unwrap();
    for _ in 0..10 { rt.tick(1.0/30.0); }
    for sys in &rt.particles {
        let mut n=0u32; let mut asz=0.0; let mut aal=0.0;
        let mut ar=0.0; let mut ag=0.0; let mut ab=0.0;
        let mut min_sy=f32::MAX; let mut max_sy=f32::MIN;
        let ow=rt.ortho_width; let oh=rt.ortho_height;
        let sc = sys.scale[0].abs().max(sys.scale[1].abs()).max(0.01);
        for p in sys.alive() {
            let cam = sys.local_to_camera(p.pos);
            let [_, sy] = wallengine_we::camera_to_screen(cam[0], cam[1], ow, oh);
            min_sy=min_sy.min(sy); max_sy=max_sy.max(sy);
            asz += p.size * sc;
            aal += p.alpha;
            ar += p.color[0]; ag += p.color[1]; ab += p.color[2];
            n+=1;
        }
        if n==0 { continue; }
        println!(
            "«{}» blend={} overbright={:.1} n={n} mean_size_px={:.1} mean_alpha={:.3} mean_rgb=({:.2},{:.2},{:.2}) y=[{:.0}..{:.0}] speed_scale note: ov in log",
            sys.name, sys.blending, sys.overbright,
            asz/n as f32, aal/n as f32, ar/n as f32, ag/n as f32, ab/n as f32,
            min_sy, max_sy
        );
    }
}
