//! Per-scene fidelity audit: what scene.json declares vs what walld actually
//! loads and draws. Catches silently-dropped layers (the class of bug where a
//! render "looks fine" but the subject is missing).
//!
//! Usage: audit_scene <workshop-id> [sim_secs]

use serde_json::Value;

fn main() {
    env_logger::init();
    let id = std::env::args().nth(1).expect("workshop id");
    let sim: f32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3.0);
    let dir = wallengine_we::workshop_dir().join(&id);

    let mut rt = match wallengine_we::WeSceneRuntime::load(&dir, &id, &id) {
        Ok(r) => r,
        Err(e) => {
            println!("{id}\tLOAD_FAIL\t{e}");
            return;
        }
    };
    for _ in 0..((sim * 30.0).max(1.0) as u32) {
        rt.tick(1.0 / 30.0);
    }

    // Declared objects straight from scene.json.
    let scene_path = {
        let cached = wallengine_we::we_cache_dir().join(&id).join("scene.json");
        if cached.is_file() { cached } else { dir.join("scene.json") }
    };
    let raw: Value = std::fs::read_to_string(&scene_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let objs = raw
        .get("objects")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut want_img = 0;
    let mut want_ptcl = 0;
    let mut want_text = 0;
    for o in &objs {
        if o.get("text").is_some() {
            want_text += 1;
        } else if o
            .get("particle")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty() && s != "null")
            .unwrap_or(false)
        {
            want_ptcl += 1;
        } else if o
            .get("image")
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty() && s != "null")
            .unwrap_or(false)
        {
            want_img += 1;
        }
    }

    let draws = rt.scene_draw_list();
    let (mut d_img, mut d_ptcl, mut d_text) = (0, 0, 0);
    for d in &draws {
        match d {
            wallengine_we::SceneDrawItem::Image(_) => d_img += 1,
            wallengine_we::SceneDrawItem::Particle(_) => d_ptcl += 1,
            wallengine_we::SceneDrawItem::Text(_) => d_text += 1,
        }
    }
    let live_particles: usize = rt.particles.iter().map(|p| p.alive().count()).sum();

    // Loss = declared drawable objects that never became runtime layers.
    let img_loss = want_img as i64 - rt.images.len() as i64;
    let ptcl_loss = want_ptcl as i64 - rt.particles.len() as i64;
    let text_loss = want_text as i64 - rt.texts.len() as i64;
    let flag = if img_loss > 0 || ptcl_loss > 0 || text_loss > 0 {
        "LOSS"
    } else {
        "ok"
    };

    println!(
        "{id}\t{flag}\timg {}/{}\tptcl {}/{}\ttext {}/{}\tdrawn(i/p/t) {}/{}/{}\tliveP {}\ttitle «{}»",
        rt.images.len(),
        want_img,
        rt.particles.len(),
        want_ptcl,
        rt.texts.len(),
        want_text,
        d_img,
        d_ptcl,
        d_text,
        live_particles,
        rt.title,
    );
}
