//! Wallpaper Engine coordinate spaces.
//!
//! Authored scene positions are bottom-left, Y-up. Camera space is centered,
//! also Y-up. Image/UI pixels are top-left, Y-down. Keep those conversions
//! separate: reflecting scene origins separates cropped parts of one image.

/// Screen space (top-left, y-down) → camera space (center, y-up).
#[inline]
pub fn screen_to_camera(sx: f32, sy: f32, ortho_w: f32, ortho_h: f32) -> [f32; 2] {
    [sx - ortho_w * 0.5, ortho_h * 0.5 - sy]
}

/// Camera space → screen space.
#[inline]
pub fn camera_to_screen(cx: f32, cy: f32, ortho_w: f32, ortho_h: f32) -> [f32; 2] {
    [cx + ortho_w * 0.5, ortho_h * 0.5 - cy]
}

/// Authored scene origin (bottom-left, Y-up) → centered camera space.
#[inline]
pub fn origin_to_camera(origin: [f32; 3], ortho_w: f32, ortho_h: f32) -> [f32; 3] {
    [origin[0] - ortho_w * 0.5, origin[1] - ortho_h * 0.5, origin[2]]
}

/// Cover scale + letterbox offset: how the ortho canvas maps onto a viewport.
/// Returns `(scale, offset_x, offset_y)` in pixels (offset is top-left of the drawn canvas).
#[inline]
pub fn cover_fit(ortho_w: f32, ortho_h: f32, vw: f32, vh: f32) -> (f32, f32, f32) {
    let s = f32::max(vw / ortho_w.max(1.0), vh / ortho_h.max(1.0));
    let drawn_w = ortho_w * s;
    let drawn_h = ortho_h * s;
    let ox = (vw - drawn_w) * 0.5;
    let oy = (vh - drawn_h) * 0.5;
    (s, ox, oy)
}

/// Column-major mat4: camera space → NDC, covering the viewport (WE-style ortho + cover).
///
/// Camera (0,0) → viewport center. +cam_y → up on screen.
pub fn camera_to_ndc_mvp(ortho_w: f32, ortho_h: f32, vw: i32, vh: i32) -> [f32; 16] {
    let (vw, vh) = (vw as f32, vh as f32);
    let (s, _, _) = cover_fit(ortho_w, ortho_h, vw, vh);
    // screen_x = vw/2 + cam_x * s  →  ndc_x = cam_x * (2s/vw)
    // screen_y_from_top = vh/2 - cam_y * s  →  ndc_y = cam_y * (2s/vh)
    let a = 2.0 * s / vw.max(1.0);
    let b = 2.0 * s / vh.max(1.0);
    [
        a, 0.0, 0.0, 0.0, //
        0.0, b, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ]
}

/// Model matrix: unit quad [-0.5,0.5]² centered at camera-space origin, scaled to `size`,
/// rotated by `angle_z` radians about Z.
///
/// Scene and camera axes have the same handedness; preserve Z rotation.
pub fn model_center_camera(origin: [f32; 2], size: [f32; 2], angle: f32) -> [f32; 16] {
    let c = angle.cos();
    let s = angle.sin();
    let sx = size[0];
    let sy = size[1];
    // M = T * R * S  (column-major)
    [
        c * sx,
        s * sx,
        0.0,
        0.0,
        -s * sy,
        c * sy,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        origin[0],
        origin[1],
        0.0,
        1.0,
    ]
}

pub fn mul_mat4(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut o = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            o[col * 4 + row] = a[row] * b[col * 4]
                + a[4 + row] * b[col * 4 + 1]
                + a[8 + row] * b[col * 4 + 2]
                + a[12 + row] * b[col * 4 + 3];
        }
    }
    o
}

/// Camera-space point → normalized viewport UV (0..1, y down) for the soft point shader.
pub fn camera_to_viewport_uv(
    cam_x: f32,
    cam_y: f32,
    ortho_w: f32,
    ortho_h: f32,
    vw: f32,
    vh: f32,
) -> [f32; 2] {
    let (s, _, _) = cover_fit(ortho_w, ortho_h, vw, vh);
    let screen_x = vw * 0.5 + cam_x * s;
    let screen_y = vh * 0.5 - cam_y * s;
    [screen_x / vw.max(1.0), screen_y / vh.max(1.0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authored_positions_are_y_up_but_pointer_pixels_are_y_down() {
        assert_eq!(origin_to_camera([25.0,75.0,3.0],100.0,100.0),[-25.0,25.0,3.0]);
        assert_eq!(screen_to_camera(25.0,25.0,100.0,100.0),[-25.0,25.0]);
        assert_eq!(camera_to_screen(-25.0,25.0,100.0,100.0),[25.0,25.0]);
    }
    #[test]
    fn puppet_and_quad_share_translation_rotation_and_reflection() {
        let mesh=crate::scene::PuppetMesh {
            vertices:vec![1.0,2.0,0.0,0.0,0.0],indices:vec![0],attachments:Default::default(),..Default::default()
        };
        let origin=[12.0,35.0];let scale=[-2.0,3.0];let angle=std::f32::consts::FRAC_PI_2;
        let tris=mesh.to_camera_tris(origin,scale,angle);
        let m=model_center_camera(origin,scale,angle);
        let expected=[m[0]+2.0*m[4]+m[12],m[1]+2.0*m[5]+m[13]];
        assert!((tris[0][0]-expected[0]).abs()<1e-5);
        assert!((tris[0][1]-expected[1]).abs()<1e-5);
        assert!((tris[0][0]-6.0).abs()<1e-5);
        assert!((tris[0][1]-33.0).abs()<1e-5);
    }
}
