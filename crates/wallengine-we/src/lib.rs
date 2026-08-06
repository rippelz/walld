//! Wallpaper Engine content access for wallstudio / walld.

pub mod paths;
pub mod pkg;
pub mod project;
pub mod tex;
pub mod scan;
pub mod player;

pub use paths::*;
pub use pkg::{PkgArchive, PkgError, ensure_unpacked};
pub use project::{Project, WallpaperType};
pub use tex::{TexError, decode_tex_to_rgba};
pub use scan::{WeEntry, WeSource, format_size, scan_all};
pub use player::{
    PlayBackend, PlayRequest, PlayerError, RuntimeStatus, detect_backends, discover_monitors, discover_monitors_info, MonitorInfo, play,
    status_snapshot, stop_all,
};
