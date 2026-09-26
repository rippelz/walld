// Synthetic MDLV0023 / MDLS0004 / MDLA0006; no workshop assets required.
fn u32le(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_le_bytes());
}
fn floats(out: &mut Vec<u8>, values: &[f32]) {
    for n in values {
        out.extend(n.to_le_bytes());
    }
}

pub fn animated_mesh() -> Vec<u8> {
    let mut out = b"MDLV0023\0".to_vec();
    u32le(&mut out, 0x0180000f); // mesh format flags, NOT a byte stride
    u32le(&mut out, 3 * 80);
    for position in [[10.0, -15.0, 0.0], [12.0, -15.0, 0.0], [10.0, -13.0, 0.0]] {
        floats(&mut out, &position);
        floats(&mut out, &[0.0; 7]);
        for index in [0, 1, 0, 0] {
            u32le(&mut out, index);
        }
        floats(&mut out, &[0.25, 0.75, 0.0, 0.0]);
        floats(&mut out, &[0.5, 0.5]);
    }
    u32le(&mut out, 6);
    for index in [0u16, 1, 2] {
        out.extend(index.to_le_bytes());
    }
    out.extend(b"MDLS0004\0");
    let skeleton_end = out.len();
    u32le(&mut out, 0);
    u32le(&mut out, 2);
    // Child first deliberately checks that hierarchy resolution is not
    // dependent on file order. Bind positions are model-space, not bone-local.
    for (parent, translation) in [(1, [0.0, 5.0, 0.0]), (u32::MAX, [10.0, -20.0, 0.0])] {
        out.push(0);
        u32le(&mut out, 1);
        u32le(&mut out, parent);
        u32le(&mut out, 64);
        floats(
            &mut out,
            &[
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                0.0,
                0.0,
                0.0,
                1.0,
                0.0,
                translation[0],
                translation[1],
                translation[2],
                1.0,
            ],
        );
        out.push(0);
    }
    let end = out.len() as u32;
    out[skeleton_end..skeleton_end + 4].copy_from_slice(&end.to_le_bytes());
    out.extend(b"MDLA0006\0");
    let animation_end = out.len();
    u32le(&mut out, 0);
    u32le(&mut out, 2);
    for id in [42, 99] {
        u32le(&mut out, id);
        u32le(&mut out, 0);
        out.extend("骨 animation\0loop\0".as_bytes());
        floats(&mut out, &[1.0]); // fps
        u32le(&mut out, 2); // two frames plus closing sample
        u32le(&mut out, 0);
        u32le(&mut out, 2); // tracks in bone order
        for bone in 0..2 {
            u32le(&mut out, 0); // uncompressed TRS
            u32le(&mut out, 3 * 36);
            for frame in 0..3 {
                let active = frame == 1;
                let mut t = if bone == 0 {
                    [0.0, 5.0, 0.0]
                } else {
                    [10.0, -20.0, 0.0]
                };
                let angle = if id == 42 && bone == 0 && active {
                    std::f32::consts::FRAC_PI_2
                } else {
                    0.0
                };
                if bone == 1 && active {
                    if id == 42 {
                        t[1] += 8.0;
                    } else {
                        t[0] += 4.0;
                    }
                }
                floats(&mut out, &t);
                floats(&mut out, &[0.0, 0.0, angle]);
                floats(&mut out, &[1.0; 3]);
            }
        }
        out.extend([0; 35]);
    }
    let end = out.len() as u32;
    out[animation_end..animation_end + 4].copy_from_slice(&end.to_le_bytes());
    out
}
