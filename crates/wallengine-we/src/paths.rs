use std::path::PathBuf;

pub fn steam_root() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    let candidates = [
        PathBuf::from(&home).join(".local/share/Steam"),
        PathBuf::from(&home).join(".steam/steam"),
        PathBuf::from(&home).join(".steam/debian-installation"),
    ];
    candidates.iter().find(|p| p.join("steamapps").is_dir()).cloned().unwrap_or_else(|| candidates[0].clone())
}

pub fn workshop_dir() -> PathBuf {
    steam_root().join("steamapps/workshop/content/431960")
}

pub fn we_install_dir() -> PathBuf {
    steam_root().join("steamapps/common/wallpaper_engine")
}

pub fn we_assets_dir() -> PathBuf {
    we_install_dir().join("assets")
}

pub fn we_myprojects_dir() -> PathBuf {
    we_install_dir().join("projects/myprojects")
}

/// Cache for unpacked scene.pkg contents.
pub fn we_cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("wallengine/we");
        }
    }
    PathBuf::from(home).join(".cache/wallengine/we")
}

pub fn lwe_binary() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    let candidates = [
        PathBuf::from(&home).join(".local/bin/linux-wallpaperengine"),
        PathBuf::from("/usr/bin/linux-wallpaperengine"),
        PathBuf::from("/usr/local/bin/linux-wallpaperengine"),
        PathBuf::from(&home).join("code/linux-wallpaperengine-build/output/linux-wallpaperengine"),
        PathBuf::from("/tmp/linux-wallpaperengine/build/output/linux-wallpaperengine"),
    ];
    candidates.into_iter().find(|p| p.is_file()).or_else(|| {
        which("linux-wallpaperengine")
    })
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn mpvpaper_binary() -> Option<PathBuf> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    let local = PathBuf::from(home).join(".local/bin/mpvpaper");
    if local.is_file() {
        return Some(local);
    }
    which("mpvpaper")
}

pub fn walld_binary() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    let local = PathBuf::from(home).join(".local/bin/walld");
    if local.is_file() {
        local
    } else {
        PathBuf::from("walld")
    }
}
