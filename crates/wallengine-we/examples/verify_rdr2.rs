//! Verify RDR2 particle systems land in the correct screen-space quadrants.

fn main() {
    let id = "1932433918";
    let dir = wallengine_we::workshop_dir().join(id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, id, "rdr2").expect("load");
    let ow = rt.ortho_width;
    let oh = rt.ortho_height;
    println!("ortho {ow}x{oh} images={} particles={}", rt.images.len(), rt.particles.len());

    for img in &rt.images {
        let [sx, sy] = wallengine_we::camera_to_screen(img.origin[0], img.origin[1], ow, oh);
        println!(
            "IMG «{}» cam=({:.1},{:.1}) screen=({:.1},{:.1}) size=({:.0}x{:.0})",
            img.name, img.origin[0], img.origin[1], sx, sy, img.size[0]*img.scale[0].abs(), img.size[1]*img.scale[1].abs()
        );
    }

    // Tick a bit so particles exist
    for _ in 0..30 {
        rt.tick(1.0 / 30.0);
    }

    let mut fails = 0;
    for sys in &rt.particles {
        let [osx, osy] = wallengine_we::camera_to_screen(sys.origin_cam[0], sys.origin_cam[1], ow, oh);
        // average alive particle screen pos
        let mut n = 0u32;
        let mut ax = 0.0f32;
        let mut ay = 0.0f32;
        let mut avy = 0.0f32;
        for p in sys.alive() {
            let cam = sys.local_to_camera(p.pos);
            let [sx, sy] = wallengine_we::camera_to_screen(cam[0], cam[1], ow, oh);
            ax += sx;
            ay += sy;
            // approx screen-space vy from camera vel: local vel transformed
            // cam_y increases up; screen y increases down → screen_vy = -cam_vy
            let cam_v = {
                let ang = -sys.angle_z;
                let c = ang.cos();
                let s = ang.sin();
                let vx = p.vel[0] * sys.scale[0];
                let vy = p.vel[1] * sys.scale[1];
                let _cx = vx * c - vy * s;
                let cy = vx * s + vy * c;
                cy
            };
            avy += -cam_v; // screen-space y velocity (down positive)
            n += 1;
        }
        if n == 0 {
            println!("SYS «{}» origin_screen=({osx:.0},{osy:.0}) NO PARTICLES", sys.name);
            continue;
        }
        ax /= n as f32;
        ay /= n as f32;
        avy /= n as f32;
        let quad = match (ax > ow * 0.5, ay > oh * 0.5) {
            (false, false) => "top-left",
            (true, false) => "top-right",
            (false, true) => "bot-left",
            (true, true) => "bot-right",
        };
        let fall = if avy > 5.0 { "FALLING↓" } else if avy < -5.0 { "RISING↑" } else { "drift" };
        println!(
            "SYS «{}» origin=({osx:.0},{osy:.0}) mean_pos=({ax:.0},{ay:.0}) [{quad}] mean_screen_vy={avy:.1} {fall} n={n}",
            sys.name
        );

        // Snow must fall (positive screen-y velocity). Spawn can be mid/upper (emit above).
        if sys.name.contains("Snow") {
            if avy < 0.0 {
                println!("  FAIL: snow rising (screen_vy={avy:.1}), expected falling (positive)");
                fails += 1;
            }
            // Mean should stay on-canvas horizontally for systems whose origin is on-canvas
            if osx > 0.0 && osx < ow && (ax < -ow * 0.25 || ax > ow * 1.25) {
                println!("  FAIL: snow mean x={ax:.0} far off-canvas (origin x={osx:.0})");
                fails += 1;
            }
        }
    }

    // Image 0 must be centered full-screen
    let img = &rt.images[0];
    let [sx, sy] = wallengine_we::camera_to_screen(img.origin[0], img.origin[1], ow, oh);
    if (sx - ow * 0.5).abs() > 1.0 || (sy - oh * 0.5).abs() > 1.0 {
        println!("FAIL: bg origin screen ({sx},{sy}) != center");
        fails += 1;
    }

    // Simulate where MVP puts a particle at snow origin — NDC should match screen
    let vw = 2560.0f32;
    let vh = 1440.0f32;
    for sys in rt.particles.iter().filter(|s| s.name.contains("Snow")).take(1) {
        let cam = sys.origin_cam;
        let uv = wallengine_we::camera_to_viewport_uv(cam[0], cam[1], ow, oh, vw, vh);
        let ndc_x = uv[0] * 2.0 - 1.0;
        let ndc_y = -(uv[1] * 2.0 - 1.0); // same as point shader
        let [sx, sy] = wallengine_we::camera_to_screen(cam[0], cam[1], ow, oh);
        println!(
            "MAP check origin cam=({:.1},{:.1}) → uv=({:.3},{:.3}) ndc=({:.3},{:.3}) screen=({:.0},{:.0})",
            cam[0], cam[1], uv[0], uv[1], ndc_x, ndc_y, sx, sy
        );
        // For exact fit 2560x1440, ndc_x should equal cam_x/(ow/2)
        let expect_ndc_x = cam[0] / (ow * 0.5);
        let expect_ndc_y = cam[1] / (oh * 0.5);
        println!(
            "  expect ndc from cam=({expect_ndc_x:.3},{expect_ndc_y:.3}) got=({ndc_x:.3},{ndc_y:.3})"
        );
        if (ndc_x - expect_ndc_x).abs() > 0.02 || (ndc_y - expect_ndc_y).abs() > 0.02 {
            println!("  FAIL: NDC mapping mismatch (particles will be misplaced on screen)");
            fails += 1;
        }
    }

    if fails > 0 {
        println!("\n{fails} FAILURE(S)");
        std::process::exit(1);
    }
    println!("\nALL CHECKS PASSED");
}
