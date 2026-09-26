
fn main() {
  let id="3164376283";
  let dir=wallengine_we::workshop_dir().join(id);
  let rt=wallengine_we::WeSceneRuntime::load(&dir,id,id).unwrap();
  println!("ortho {}x{}", rt.ortho_width, rt.ortho_height);
  for l in &rt.images {
    println!("«{}» origin={:?} size={:?} scale={:?} angles={:?} solid={} compose={} effects={} visible={}",
      l.name, l.origin, l.size, l.scale, l.angles, l.solidlayer, l.composelayer, l.effect_passes.len(), l.visible);
  }
  for item in rt.scene_draw_list() {
    match item {
      wallengine_we::SceneDrawItem::Image(d) => {
        println!("draw Image idx={} origin={:?} size={:?} angle={} alpha={} compose={}",
          d.layer_index, d.origin, d.size, d.angle_z, d.alpha, d.composelayer);
      }
      wallengine_we::SceneDrawItem::Particle(p) => println!("draw Particle {:?}", p.system_index),
      wallengine_we::SceneDrawItem::Text(t) => println!("draw Text"),
    }
  }
}
