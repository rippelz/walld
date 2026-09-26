//! Wallpaper Engine content access + scene runtime for wallstudio / walld.
//!
//! Independent implementation — does not shell out to linux-wallpaperengine,
//! mpvpaper, or other external wallpaper engines.

pub mod accent;
pub mod paths;
pub mod pkg;
pub mod project;
pub mod props;
pub mod tex;
pub mod scan;
pub mod player;
pub mod assets;
pub mod blend;
pub mod glsl;
pub mod transform;
pub mod scene;
pub mod editor;
pub mod soft_preview;
pub mod preview_gif;

pub use accent::{dominant_color, dominant_color_rgba, hsv_to_rgb, rgb_to_hsv};
pub use paths::*;
pub use pkg::{PkgArchive, PkgError, ensure_unpacked};
pub use project::{parse_rgb_triplet, Project, WallpaperType};
pub use props::{
    clear_overrides, human_label, list_props, load_merged_properties, load_overrides,
    load_raw_properties, parse_value_for_prop, props_override_dir, set_override, PropDef, PropKind,
    PropValue,
};
pub use tex::{DecodedTex, TexError, TexFormat, decode_tex, decode_tex_to_rgba};
pub use scan::{WeEntry, WeSource, format_size, scan_all};
pub use player::{
    monitor_wallpaper_id, parse_monitor_paths_from_status, PlayBackend, PlayRequest, PlayerError,
    detect_backends, discover_monitors, discover_monitors_info, MonitorInfo, play, status_snapshot,
    stop_all, RuntimeStatus,
};
pub use editor::{
    delete_local_project, fork_wallpaper, is_editable_scene, is_local_project, open_project_dir,
    rename_project_title, ConstantSummary, EditableScene, EffectConstValue, EffectSummary,
    ForkedProject, LayerKind, LayerSummary, ParticleAddableKey, ParticleField, ParticleFieldKind,
};
pub use soft_preview::SoftPreview;
pub use preview_gif::{
    capture_and_write_preview_gif, encode_gif, set_project_preview_gif, spawn_preview_gif_job,
    GifFrame,
};
pub use assets::{AssetError, AssetResolver, TextureCache};
pub use blend::{apply_color_blend, gl_blend_mode_supported};
pub use scene::{
    ColorkeyParams, DebugClick, EffectKind, EffectValue, ImageDraw, ImageLayer, ParticleDraw,
    ParticleInstanceOverride, PuppetMesh, SceneDrawItem, SceneEffect, WeParticle,
    WeParticleSystem, WeSceneDoc, WeSceneRuntime,
};
pub use transform::{
    camera_to_ndc_mvp, camera_to_screen, camera_to_viewport_uv, cover_fit, model_center_camera,
    mul_mat4, origin_to_camera, screen_to_camera,
};
