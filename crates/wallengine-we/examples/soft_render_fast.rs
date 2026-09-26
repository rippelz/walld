//! Fast offline composite at half ortho res.
fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "3448877775".into());
    let out = std::env::args().nth(2).unwrap_or_else(|| format!("/tmp/we_{id}_fast.png"));
    let dir = wallengine_we::workshop_dir().join(&id);
    let mut rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).expect("load");
    // Extra ticks so SceneScript init() createLayer (audio bars) has run.
    for _ in 0..12 { rt.tick(1.0/60.0); }
    let ow = rt.ortho_width;
    let oh = rt.ortho_height;
    let scale = 0.35f32;
    let vw = (ow * scale) as u32;
    let vh = (oh * scale) as u32;
    let mut buf = vec![0u8; (vw*vh*4) as usize];
    eprintln!("fast render {vw}x{vh} from {ow}x{oh} images={} puppets={}",
        rt.images.len(), rt.images.iter().filter(|i| i.puppet.is_some()).count());

    for d in rt.image_draws() {
        let layer = &rt.images[d.layer_index];
        let Some(ref tex) = layer.rgba else { continue };
        if d.has_puppet {
            if let Some(ref mesh) = layer.puppet {
                let tris = mesh.to_camera_tris_crop(
                    [layer.origin[0], layer.origin[1]],
                    [layer.scale[0], layer.scale[1]],
                    layer.angles[2],
                    layer.crop_offset,
                    layer.size,
                );
                for tri in tris.chunks(3) {
                    if tri.len() < 3 { continue; }
                    let pts: Vec<(f32,f32,f32,f32)> = tri.iter().map(|v| {
                        let sx = (vw as f32)*0.5 + v[0]*scale;
                        let sy = (vh as f32)*0.5 - v[1]*scale;
                        (sx, sy, v[2], v[3])
                    }).collect();
                    let minx = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min).floor().max(0.0) as i32;
                    let maxx = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max).ceil().min(vw as f32) as i32;
                    let miny = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min).floor().max(0.0) as i32;
                    let maxy = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max).ceil().min(vh as f32) as i32;
                    let area = edge(pts[0].0, pts[0].1, pts[1].0, pts[1].1, pts[2].0, pts[2].1);
                    if area.abs() < 1e-3 { continue; }
                    for py in miny..maxy {
                        for px in minx..maxx {
                            let w0 = edge(pts[1].0, pts[1].1, pts[2].0, pts[2].1, px as f32+0.5, py as f32+0.5) / area;
                            let w1 = edge(pts[2].0, pts[2].1, pts[0].0, pts[0].1, px as f32+0.5, py as f32+0.5) / area;
                            let w2 = edge(pts[0].0, pts[0].1, pts[1].0, pts[1].1, px as f32+0.5, py as f32+0.5) / area;
                            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 { continue; }
                            let u = w0*pts[0].2 + w1*pts[1].2 + w2*pts[2].2;
                            let v = w0*pts[0].3 + w1*pts[1].3 + w2*pts[2].3;
                            let (r,g,b,a) = sample(tex, u, v);
                            if a < 8 { continue; }
                            blend(&mut buf, vw, px, py, r, g, b, a as f32 / 255.0);
                        }
                    }
                }
                continue;
            }
        }
        let half_w = d.size[0]*0.5*scale;
        let half_h = d.size[1]*0.5*scale;
        let cx = vw as f32 * 0.5 + d.origin[0]*scale;
        let cy = vh as f32 * 0.5 - d.origin[1]*scale;
        let x0 = (cx-half_w).floor().max(0.0) as i32;
        let y0 = (cy-half_h).floor().max(0.0) as i32;
        let x1 = (cx+half_w).ceil().min(vw as f32) as i32;
        let y1 = (cy+half_h).ceil().min(vh as f32) as i32;
        let (cu, cv) = tex.content_uv_scale();
        for py in y0..y1 {
            for px in x0..x1 {
                let u = ((px as f32 + 0.5 - (cx - half_w)) / (half_w*2.0)).clamp(0.0,1.0);
                let v = ((py as f32 + 0.5 - (cy - half_h)) / (half_h*2.0)).clamp(0.0,1.0);
                let (r,g,b,a) = sample(tex, u*cu, v*cv);
                if a < 8 { continue; }
                blend(&mut buf, vw, px, py, r, g, b, a as f32 / 255.0);
            }
        }
    }
    image::save_buffer(&out, &buf, vw, vh, image::ColorType::Rgba8).unwrap();
    eprintln!("wrote {out}");
}
fn edge(ax:f32,ay:f32,bx:f32,by:f32,cx:f32,cy:f32)->f32 { (cx-ax)*(by-ay)-(cy-ay)*(bx-ax) }
fn sample(tex:&wallengine_we::DecodedTex,u:f32,v:f32)->(u8,u8,u8,u8) {
    let w=tex.width.max(1); let h=tex.height.max(1);
    let x=((u.clamp(0.0,1.0))*(w as f32 - 1.0)) as u32;
    let y=((v.clamp(0.0,1.0))*(h as f32 - 1.0)) as u32;
    let i=((y*w+x)*4) as usize;
    if i+3>=tex.rgba.len() { return (0,0,0,0); }
    (tex.rgba[i],tex.rgba[i+1],tex.rgba[i+2],tex.rgba[i+3])
}
fn blend(buf:&mut [u8], vw:u32, x:i32, y:i32, r:u8,g:u8,b:u8,a:f32) {
    if x<0||y<0||x as u32>=vw { return; }
    let i=((y as u32*vw+x as u32)*4) as usize;
    if i+3>=buf.len(){return;}
    let ia=1.0-a;
    buf[i]=(r as f32*a + buf[i] as f32*ia) as u8;
    buf[i+1]=(g as f32*a + buf[i+1] as f32*ia) as u8;
    buf[i+2]=(b as f32*a + buf[i+2] as f32*ia) as u8;
    buf[i+3]=255;
}
