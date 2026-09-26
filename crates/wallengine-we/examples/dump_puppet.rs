
fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| "3448877775".into());
    let dir = wallengine_we::workshop_dir().join(&id);
    let rt = wallengine_we::WeSceneRuntime::load(&dir, &id, &id).unwrap();
    for img in &rt.images {
        if let Some(ref p) = img.puppet {
            println!("puppet «{}» verts={} indices={} origin_cam=({:.1},{:.1}) scale=({:.2},{:.2})",
                img.name, p.vertices.len()/5, p.indices.len(), img.origin[0], img.origin[1], img.scale[0], img.scale[1]);
            // AABB of mesh in camera
            let tris = p.to_camera_tris([img.origin[0], img.origin[1]], [img.scale[0], img.scale[1]], img.angles[2]);
            let mut minx=f32::MAX; let mut maxx=f32::MIN; let mut miny=f32::MAX; let mut maxy=f32::MIN;
            for t in &tris {
                minx=minx.min(t[0]); maxx=maxx.max(t[0]);
                miny=miny.min(t[1]); maxy=maxy.max(t[1]);
            }
            let ow=rt.ortho_width; let oh=rt.ortho_height;
            let [sx0,sy0]=wallengine_we::camera_to_screen(minx,maxy,ow,oh);
            let [sx1,sy1]=wallengine_we::camera_to_screen(maxx,miny,ow,oh);
            println!("  cam AABB x=[{minx:.0}..{maxx:.0}] y=[{miny:.0}..{maxy:.0}]");
            println!("  screen AABB x=[{sx0:.0}..{sx1:.0}] y=[{sy0:.0}..{sy1:.0}] n_tris={}", tris.len()/3);
            // sample first few verts
            for i in 0..3.min(p.indices.len()) {
                let idx=p.indices[i] as usize;
                let b=idx*5;
                println!("  v{} raw=({:.1},{:.1}) uv=({:.3},{:.3})", i, p.vertices[b], p.vertices[b+1], p.vertices[b+3], p.vertices[b+4]);
            }
        }
    }
}
