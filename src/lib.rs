//! Shared walld engine: rendering, WE content loading, config.
//! The binary adds Wayland output + IPC on top (see main.rs).

pub mod config;
pub mod image;
pub mod render;
pub mod video;
pub mod video_mpv;
pub mod we_runtime;

pub mod effects;
