//! Steam-style system tray (StatusNotifierItem) for wallstudio.
//!
//! The library window minimizes here instead of quitting, and the right-click
//! menu controls playback (pause/mute/stop) plus open/quit — the whole app
//! stays resident in the tray like Steam.

use crate::Message;
use iced::futures::channel::mpsc::{UnboundedReceiver, UnboundedSender};
use iced::futures::stream;
use iced::futures::StreamExt;
use iced::Subscription;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// Right-click menu actions, forwarded to the iced update loop.
#[derive(Debug, Clone, Copy)]
pub enum TrayEvent {
    /// Show the library window again (also on left/middle click).
    Open,
    TogglePause,
    ToggleMute,
    Stop,
    Quit,
}

/// Playback state the tray menu reflects (Pause ↔ Resume, Mute ↔ Unmute).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TrayState {
    pub paused: bool,
    pub muted: bool,
}

/// The StatusNotifierItem. ksni runs it on a background thread.
pub struct TrayIcon {
    tx: UnboundedSender<TrayEvent>,
    pub state: TrayState,
}

impl TrayIcon {
    pub fn new(tx: UnboundedSender<TrayEvent>) -> Self {
        Self {
            tx,
            state: TrayState::default(),
        }
    }
}

impl ksni::Tray for TrayIcon {
    fn id(&self) -> String {
        "wallstudio".into()
    }

    fn title(&self) -> String {
        "wallstudio — Wallpaper Engine".into()
    }

    fn category(&self) -> ksni::Category {
        ksni::Category::ApplicationStatus
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![TRAY_ICON.clone()]
    }

    // Left click shows the app, like Steam.
    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.unbounded_send(TrayEvent::Open);
    }

    // Middle click shows the app too.
    fn secondary_activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.unbounded_send(TrayEvent::Open);
    }

    fn menu(&self) -> Vec<ksni::menu::MenuItem<Self>> {
        use ksni::menu::{MenuItem, StandardItem};
        let paused = self.state.paused;
        let muted = self.state.muted;
        vec![
            MenuItem::Standard(StandardItem {
                label: "Open wallstudio".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(TrayEvent::Open);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: if paused { "Resume" } else { "Pause" }.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(TrayEvent::TogglePause);
                }),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: if muted { "Unmute" } else { "Mute" }.into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(TrayEvent::ToggleMute);
                }),
                ..Default::default()
            }),
            MenuItem::Standard(StandardItem {
                label: "Stop wallpaper".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(TrayEvent::Stop);
                }),
                ..Default::default()
            }),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Quit wallstudio".into(),
                activate: Box::new(|t: &mut Self| {
                    let _ = t.tx.unbounded_send(TrayEvent::Quit);
                }),
                ..Default::default()
            }),
        ]
    }
}

/// Spawn the tray on a background thread. Returns `None` when the desktop has
/// no StatusNotifierHost/Watcher (no tray bar) — the app then just runs
/// without one and keeps its old close-to-exit behaviour.
pub fn spawn(tx: UnboundedSender<TrayEvent>) -> Option<ksni::blocking::Handle<TrayIcon>> {
    use ksni::blocking::TrayMethods;
    match TrayIcon::new(tx).spawn() {
        Ok(handle) => {
            log::info!("tray icon registered");
            Some(handle)
        }
        Err(e) => {
            log::warn!("tray icon unavailable — {e}");
            None
        }
    }
}

/// The embedded app icon, decoded to ARGB32 for the tray pixmap.
static TRAY_ICON: std::sync::LazyLock<ksni::Icon> = std::sync::LazyLock::new(|| {
    let img = image::load_from_memory_with_format(
        include_bytes!("../../../assets/icons/wallstudio.png"),
        image::ImageFormat::Png,
    )
    .expect("embedded wallstudio tray icon is valid");
    let small = image::imageops::resize(&img, 128, 128, image::imageops::FilterType::Lanczos3);
    let mut data = small.into_raw();
    for px in data.chunks_exact_mut(4) {
        px.rotate_right(1); // RGBA -> ARGB
    }
    ksni::Icon {
        width: 128,
        height: 128,
        data,
    }
});

/// Hashable carrier for the tray event receiver so the subscription stream can
/// own it across redraws (iced subscriptions are identified by hash).
#[derive(Clone)]
pub struct TrayStream {
    rx: Arc<Mutex<Option<UnboundedReceiver<TrayEvent>>>>,
}

impl Default for TrayStream {
    fn default() -> Self {
        Self {
            rx: Arc::new(Mutex::new(None)),
        }
    }
}

impl TrayStream {
    pub fn new(rx: UnboundedReceiver<TrayEvent>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(Some(rx))),
        }
    }
}

impl Hash for TrayStream {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "wallstudio::tray".hash(state);
    }
}

/// Subscription that relays tray menu/click events into the app.
pub fn subscription(rx: TrayStream) -> Subscription<Message> {
    Subscription::run_with(rx, |stream| {
        let inner = stream.rx.clone();
        stream::unfold(
            inner,
            |inner: Arc<Mutex<Option<UnboundedReceiver<TrayEvent>>>>| async move {
                let mut rx = inner.lock().unwrap().take()?;
                let ev = rx.next().await?;
                let seed = Arc::new(Mutex::new(Some(rx)));
                Some((Message::Tray(ev), seed))
            },
        )
    })
}
