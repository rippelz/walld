use wallengine_we::{SceneDrawItem, WeSceneRuntime};

#[test]
fn reflected_geometry_and_effect_fallbacks_preserve_authored_values() {
    let root = std::env::temp_dir().join(format!("walld-scene-pipeline-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut tex = b"TEXV0005\0TEXI0001\0".to_vec();
    for n in [0u32, 0, 4, 4, 4, 4, 0] {
        tex.extend(n.to_le_bytes());
    }
    tex.extend(b"TEXB0001\0");
    for n in [1u32, 1, 4, 4, 64] {
        tex.extend(n.to_le_bytes());
    }
    tex.extend([255, 0, 0, 255].repeat(16));
    std::fs::write(root.join("color.tex"), tex).unwrap();
    std::fs::write(root.join("model.json"), r#"{"material":"material.json"}"#).unwrap();
    std::fs::write(
        root.join("material.json"),
        r#"{"passes":[{"shader":"genericimage","textures":["color.tex"]}]}"#,
    )
    .unwrap();
    std::fs::write(root.join("scene.json"),r#"{
      "general":{"orthogonalprojection":{"width":8,"height":8}},
      "objects":[{"id":1,"image":"model.json","origin":"4 6 0","size":"4 4","scale":"-2 1 1","alpha":0.6,
        "effects":[{"file":"effects/opacity/effect.json"},{"file":"effects/waterflow/effect.json"},{"file":"effects/colorkey/effect.json"}]}]
    }"#).unwrap();
    let mut runtime = WeSceneRuntime::load(&root, "synthetic-scene-pipeline", "fixture").unwrap();
    runtime.tick(0.0);
    let SceneDrawItem::Image(mut draw) = runtime.scene_draw_list().remove(0) else {
        panic!("expected image");
    };
    assert_eq!(draw.size, [-8.0, 4.0]);
    assert_eq!(draw.origin, [0.0, 2.0]);
    assert_eq!(draw.alpha, 0.6);
    assert!(draw.has_opacity && draw.has_waterflow && draw.colorkey.is_some());
    // A failed/unrelated effect must retain the dedicated fallback.
    draw.suppress_applied_effects(&["effects/tint/effect.json".into()]);
    assert!(draw.has_opacity && draw.has_waterflow && draw.colorkey.is_some());
    draw.suppress_applied_effects(&["effects/opacity/effect.json".into()]);
    assert!(!draw.has_opacity && draw.has_waterflow && draw.colorkey.is_some());
    draw.suppress_applied_effects(&[
        "effects/waterflow/effect.json".into(),
        "effects/colorkey/effect.json".into(),
    ]);
    assert!(!draw.has_waterflow && draw.colorkey.is_none());
    // Atlas frame window must be preserved relative to visible content when
    // a padded texture becomes an unpadded effect output.
    draw.uv_scale = [0.25, 0.125];
    draw.uv_offset = [0.25, 0.125];
    draw.rebase_texture_uv((0.5, 0.25), (1.0, 1.0));
    assert_eq!(draw.uv_scale, [0.5, 0.5]);
    assert_eq!(draw.uv_offset, [0.5, 0.5]);
    std::fs::remove_dir_all(root).unwrap();
}
