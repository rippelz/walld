mod support {
    pub mod puppet;
}
use serde_json::json;
use wallengine_we::scene::PuppetMesh;

fn mesh() -> PuppetMesh {
    PuppetMesh::parse(&support::puppet::animated_mesh(), "synthetic.mdl").unwrap()
}
fn near(actual: f32, expected: f32) {
    assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
}

#[test]
fn hierarchical_weighted_skinning_uses_inverse_bind_and_closing_frame() {
    let mut mesh = mesh();
    let rest = mesh.vertices.clone();
    mesh.set_animation_layers(&json!([{"animation":42}]));
    for (a, b) in mesh.vertices.iter().zip(&rest) {
        near(*a, *b);
    }
    mesh.tick(1.0);
    near(mesh.vertices[0], 10.0);
    near(mesh.vertices[1], -7.0);
    // 25% child rotation + 75% root translation around the child's bind pivot.
    near(mesh.vertices[5], 11.5);
    near(mesh.vertices[6], -6.5);
    assert_eq!(&mesh.vertices[8..10], &rest[8..10]); // UVs never deform
    mesh.tick(0.5);
    near(mesh.vertices[1], -11.0); // interpolate back toward closing sample
    mesh.tick(0.5);
    for (a, b) in mesh.vertices.iter().zip(&rest) {
        near(*a, *b);
    }
    for _ in 0..10 {
        mesh.tick(1.0);
    }
    for (a, b) in mesh.vertices.iter().zip(&rest) {
        near(*a, *b);
    } // no accumulated deformation
}

#[test]
fn authored_clip_ids_rate_visibility_blend_and_additive_layers_are_respected() {
    let mut mesh = mesh();
    mesh.set_animation_layers(&json!([
        {"animation":42,"rate":2.0,"blend":0.5},
        {"animation":99,"rate":2.0,"blend":0.5,"additive":true},
        {"animation":42,"visible":false}
    ]));
    mesh.tick(0.5);
    near(mesh.vertices[0], 12.0);
    near(mesh.vertices[1], -11.0);
    mesh.set_animation_layers(&json!([{"animation":99,"rate":0.0}]));
    mesh.tick(100.0);
    near(mesh.vertices[0], 10.0);
    near(mesh.vertices[1], -15.0);
    mesh.set_animation_layers(&json!([{"animation":123456}]));
    mesh.tick(1.0);
    near(mesh.vertices[1], -15.0);
}

#[test]
fn broken_animation_data_keeps_the_rest_mesh_without_panicking() {
    let bytes = support::puppet::animated_mesh();
    let start = bytes.windows(9).position(|w| w == b"MDLS0004\0").unwrap();
    // Every truncated skeleton/animation prefix must fail safely.
    for end in start + 9..bytes.len() {
        let mesh = PuppetMesh::parse(&bytes[..end], "truncated.mdl").unwrap();
        assert_eq!(mesh.vertices.len(), 15);
    }
    let mut cyclic = bytes.clone();
    // Make the root point back at the child.
    let root_parent = start + 17 + 78 + 5;
    cyclic[root_parent..root_parent + 4].copy_from_slice(&0u32.to_le_bytes());
    let mut mesh = PuppetMesh::parse(&cyclic, "cyclic.mdl").unwrap();
    mesh.set_animation_layers(&json!([{"animation":42}]));
    mesh.tick(1.0);
    near(mesh.vertices[1], -15.0);
}

#[test]
fn scene_runtime_advances_authored_puppet_animation_layers() {
    let root = std::env::temp_dir().join(format!("walld-puppet-animation-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let mut tex = b"TEXV0005\0TEXI0001\0".to_vec();
    for n in [0u32, 0, 4, 4, 4, 4, 0] {
        tex.extend(n.to_le_bytes());
    }
    tex.extend(b"TEXB0001\0");
    for n in [1u32, 1, 4, 4, 64] {
        tex.extend(n.to_le_bytes());
    }
    tex.extend([255, 255, 255, 255].repeat(16));
    std::fs::write(root.join("color.tex"), tex).unwrap();
    std::fs::write(root.join("puppet.mdl"), support::puppet::animated_mesh()).unwrap();
    std::fs::write(
        root.join("model.json"),
        r#"{"material":"material.json","puppet":"puppet.mdl"}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("material.json"),
        r#"{"passes":[{"shader":"genericimage","textures":["color.tex"]}]}"#,
    )
    .unwrap();
    std::fs::write(
        root.join("scene.json"),
        json!({
            "general":{"orthogonalprojection":{"width":100,"height":100}},
            "objects":[{"id":1,"image":"model.json","origin":"50 50 0","size":"4 4",
            "animationlayers":[{"animation":42,"rate":1.0,"blend":1.0,"visible":true}]}]
        })
        .to_string(),
    )
    .unwrap();
    let mut rt =
        wallengine_we::WeSceneRuntime::load(&root, "synthetic-puppet-animation", "fixture")
            .unwrap();
    near(rt.images[0].puppet.as_ref().unwrap().vertices[1], -15.0);
    rt.tick(1.0);
    near(rt.images[0].puppet.as_ref().unwrap().vertices[1], -7.0);
    assert_eq!(rt.images[0].origin, [0.0; 3]); // no scene-coordinate patch
    rt.tick(1.0);
    near(rt.images[0].puppet.as_ref().unwrap().vertices[1], -15.0);
    std::fs::remove_dir_all(root).unwrap();
}
