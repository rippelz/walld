//! Download activity comes from Steam's staging directories and install records.
//! File lengths are deliberately not treated as bytes received: Steam preallocates.
use crate::workshop::{self, WorkshopItem};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Subscribing,
    Waiting,
    SteamClosed,
    Downloading,
    Installing,
    Failed(String),
}

impl State {
    pub fn label(&self) -> &str {
        match self {
            Self::Subscribing => "Subscribing…",
            Self::Waiting => "Waiting / paused in Steam",
            Self::SteamClosed => "Start Steam to download",
            Self::Downloading => "Downloading…",
            Self::Installing => "Installing…",
            Self::Failed(_) => "Download could not start",
        }
    }

    pub fn animates(&self) -> bool {
        matches!(
            self,
            Self::Subscribing | Self::Downloading | Self::Installing
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Download {
    pub item: WorkshopItem,
    pub state: State,
}

fn state_path() -> PathBuf {
    let root = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/state")
        });
    root.join("wallstudio/downloads.json")
}

pub fn save(downloads: &[Download]) {
    let path = state_path();
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        std::fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(downloads)?)?;
        std::fs::rename(tmp, path)?;
        Ok(())
    })();
    if let Err(error) = result {
        log::warn!("save download queue: {error}");
    }
}

pub fn load() -> Vec<Download> {
    let mut downloads: Vec<Download> = std::fs::read(state_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    downloads.retain(|d| valid_id(&d.item.id));
    for download in &mut downloads {
        if download.state == State::Subscribing {
            download.state =
                State::Failed("Subscription was interrupted. Retry to continue.".into());
        }
    }
    downloads
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub states: Vec<(String, Option<State>)>,
}

/// Run off the UI thread. None means Steam committed the package to the library.
pub fn poll(ids: Vec<String>) -> Snapshot {
    let root = wallengine_we::workshop_dir();
    let staging = root
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("downloads/431960");
    let acf = std::fs::read_to_string(workshop::appworkshop_acf()).unwrap_or_default();
    poll_at(ids, &root, &staging, &acf, workshop::steam_running())
}

fn poll_at(ids: Vec<String>, root: &Path, staging: &Path, acf: &str, running: bool) -> Snapshot {
    let installed = workshop::acf_block(acf, "WorkshopItemsInstalled").unwrap_or("");
    let states = ids
        .into_iter()
        .filter(|id| valid_id(id))
        .map(|id| {
            let stage = staging.join(&id);
            let staged = stage.is_dir();
            let committed = workshop::acf_block(installed, &id).is_some()
                && root.join(&id).join("project.json").is_file();
            let recent = staged && recently_written(&stage);
            let state = observed_state(
                committed,
                staged,
                recent,
                running,
                root.join(&id).join("project.json").is_file(),
            );
            (id, state)
        })
        .collect();
    Snapshot { states }
}

fn observed_state(
    committed: bool,
    staged: bool,
    recent: bool,
    running: bool,
    project: bool,
) -> Option<State> {
    if committed && !staged {
        None
    } else if !running {
        Some(State::SteamClosed)
    } else if staged && recent {
        Some(State::Downloading)
    } else if !staged && project {
        Some(State::Installing)
    } else {
        Some(State::Waiting)
    }
}

fn recently_written(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    entries.flatten().any(|entry| {
        if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
            return false;
        }
        let Ok(meta) = entry.metadata() else {
            return false;
        };
        if meta.is_dir() {
            recently_written(&entry.path())
        } else {
            meta.modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age < Duration::from_secs(30))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steam_files_drive_download_through_commit() {
        let root =
            std::env::temp_dir().join(format!("wallstudio-download-test-{}", std::process::id()));
        let content = root.join("content");
        let staging = root.join("downloads");
        let id = "123".to_string();
        let check = |acf: &str| {
            poll_at(vec![id.clone()], &content, &staging, acf, true).states[0]
                .1
                .clone()
        };
        std::fs::create_dir_all(staging.join(&id)).unwrap();
        assert_eq!(check(""), Some(State::Waiting));
        std::fs::write(staging.join(&id).join("scene.pkg"), b"partial").unwrap();
        assert_eq!(check(""), Some(State::Downloading));
        std::fs::create_dir_all(content.join(&id)).unwrap();
        std::fs::write(content.join(&id).join("project.json"), b"{}").unwrap();
        assert_eq!(check(""), Some(State::Downloading));
        std::fs::remove_dir_all(staging.join(&id)).unwrap();
        assert_eq!(check(""), Some(State::Installing));
        assert_eq!(
            check(r#""WorkshopItemsInstalled" { "123" { "size" "7" } }"#),
            None
        );
        // An installed record for another wallpaper must not complete this one.
        assert_eq!(
            check(r#""WorkshopItemsInstalled" { "1234" { "size" "7" } }"#),
            Some(State::Installing)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_project_is_not_a_completed_download() {
        assert_eq!(
            observed_state(false, true, true, true, true),
            Some(State::Downloading)
        );
        assert_eq!(
            observed_state(false, false, false, true, true),
            Some(State::Installing)
        );
        assert_eq!(
            observed_state(true, true, true, true, true),
            Some(State::Downloading)
        );
        assert_eq!(observed_state(true, false, false, true, true), None);
    }

    #[test]
    fn waiting_and_offline_never_claim_active_transfer() {
        assert_eq!(
            observed_state(false, false, false, true, false),
            Some(State::Waiting)
        );
        assert_eq!(
            observed_state(false, true, false, true, false),
            Some(State::Waiting)
        );
        assert_eq!(
            observed_state(false, true, true, false, false),
            Some(State::SteamClosed)
        );
        assert!(!State::Waiting.animates());
        assert!(!State::SteamClosed.animates());
    }
}
