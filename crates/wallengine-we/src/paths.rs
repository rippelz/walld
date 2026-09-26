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

/// wallstudio forks / local editable projects (WE JSON + package tree).
///
/// `~/.local/share/wallengine/projects/<id>/`
pub fn wallengine_projects_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("wallengine/projects");
        }
    }
    PathBuf::from(home).join(".local/share/wallengine/projects")
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

/// Where to look for a sibling tool of this workspace, best first: next to the
/// running executable (a `cargo run` build finds its own siblings), the install
/// dir, then `PATH`.
fn workspace_bin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .into_iter()
        .collect();
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    dirs.push(PathBuf::from(home).join(".local/bin"));
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs
}

fn first_binary_in(dirs: &[PathBuf], name: &str) -> Option<PathBuf> {
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file())
}

/// Locate a sibling tool of this workspace (`walld`, `wallaccent`, …).
///
/// Bare-name lookup is not enough: walld is started by the compositor's
/// session, whose `PATH` often lacks `~/.local/bin` — the very place these
/// binaries install to — so `Command::new("wallaccent")` never fires there.
fn workspace_binary(name: &str) -> Option<PathBuf> {
    first_binary_in(&workspace_bin_dirs(), name)
}

pub fn walld_binary() -> PathBuf {
    workspace_binary("walld").unwrap_or_else(|| PathBuf::from("walld"))
}

/// The `wallaccent` companion, or `None` when it isn't installed.
///
/// `None` is a normal state — accenting the desktop is optional — so callers
/// can skip quietly instead of spawning a doomed process.
pub fn wallaccent_binary() -> Option<PathBuf> {
    workspace_binary("wallaccent")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn earlier_dirs_win_and_missing_tools_are_none() {
        let root = std::env::temp_dir().join(format!("walld-bin-{}", std::process::id()));
        let (first, second) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(second.join("wallaccent"), "x").unwrap();

        let dirs = vec![first.clone(), second.clone()];
        // Only the later dir has it.
        assert_eq!(
            first_binary_in(&dirs, "wallaccent"),
            Some(second.join("wallaccent"))
        );
        // A sibling of the running exe outranks the install dir.
        std::fs::write(first.join("wallaccent"), "x").unwrap();
        assert_eq!(
            first_binary_in(&dirs, "wallaccent"),
            Some(first.join("wallaccent"))
        );
        // Not installed anywhere is a clean None, never a bare name to spawn.
        assert_eq!(first_binary_in(&dirs, "walld"), None);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_install_dir_is_searched_even_when_it_is_off_path() {
        // The bug this guards: walld's session PATH has no ~/.local/bin.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
        let install = PathBuf::from(home).join(".local/bin");
        assert!(workspace_bin_dirs().contains(&install));
    }
}
