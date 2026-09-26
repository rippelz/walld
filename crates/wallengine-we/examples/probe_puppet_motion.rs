//! Audit skeletal motion directly from an unpacked scene without loading textures.
//! Usage: probe_puppet_motion <unpacked-scene-dir> [seconds]
use wallengine_we::scene::PuppetMesh;
fn main() {
    env_logger::init();
    let root = std::path::PathBuf::from(std::env::args().nth(1).expect("unpacked scene directory"));
    let seconds: f32 = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let scene: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("scene.json")).unwrap()).unwrap();
    for object in scene["objects"].as_array().unwrap() {
        let Some(model) = object["image"].as_str() else { continue; };
        let Ok(bytes) = std::fs::read(root.join(model)) else { continue; };
        let model: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let Some(path) = model["puppet"].as_str() else { continue; };
        let bytes = std::fs::read(root.join(path)).unwrap();
        let mut mesh = match PuppetMesh::parse(&bytes, path) {
            Ok(m) => m,
            Err(e) => { println!("{path}: {e}"); continue; }
        };
        mesh.set_animation_layers(&object["animationlayers"]);
        let before = mesh.vertices.clone();
        mesh.tick(seconds);
        let max_motion = mesh.vertices.chunks_exact(5).zip(before.chunks_exact(5)).map(|(a,b)| {
            ((a[0]-b[0]).powi(2)+(a[1]-b[1]).powi(2)+(a[2]-b[2]).powi(2)).sqrt()
        }).fold(0.0f32, f32::max);
        assert!(mesh.vertices.iter().all(|v| v.is_finite()), "non-finite geometry: {path}");
        println!("{path}: {:.3}px max motion after {seconds}s", max_motion);
    }
}
