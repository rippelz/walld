//! wallstudio — Wallpaper Engine browser & player for Hyprland.
//! The product is WE content. Native wallengine scenes are a secondary source.
//! Multi-window: library + scene editor(s).

mod downloads;
mod editor;
mod gifanim;
mod gizmo;
mod settings;
mod single_instance;
mod steam_session;
mod theme;
mod tray;
mod ui;
mod ui_settings;
mod workshop;

use editor::{EditorMessage, EditorSession, Tool};
use iced::keyboard::Modifiers;
use iced::window;
use iced::{Subscription, Task, Theme};
use serde_json::json;
use settings::{AccentFollow, AccentSource, PreviewAnim, Settings, VideoCap};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tray::{TrayEvent, TrayIcon, TrayState, TrayStream};
use wallengine_we as we;
use wallengine_we::{
    clear_overrides, delete_local_project, discover_monitors_info, fork_wallpaper,
    is_editable_scene, is_local_project, list_props, monitor_wallpaper_id, open_project_dir,
    play as we_play, rename_project_title, scan_all, set_override, status_snapshot, stop_all,
    MonitorInfo, PlayBackend, PlayRequest, PropDef, PropKind, PropValue, RuntimeStatus,
    WallpaperType, WeEntry, WeSource,
};
use workshop::{BrowsePage, WorkshopItem, WorkshopQuery, WorkshopSort};

fn main() -> iced::Result {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .try_init();
    if std::env::args().any(|a| a == "-h" || a == "--help") {
        println!(
            "wallstudio — Wallpaper Engine browser & player\n\n\
             usage: wallstudio [--library|--workshop|--settings]\n\n\
             \x20 --library    open the installed library (default)\n\
             \x20 --workshop   open the Steam Workshop browser\n\
             \x20 --settings   open the settings page (also Ctrl+, in app)"
        );
        return Ok(());
    }
    let primary = match single_instance::acquire_or_raise() {
        Ok(Some(primary)) => primary,
        Ok(None) => return Ok(()),
        Err(e) => {
            eprintln!("wallstudio: single-instance handoff failed: {e}");
            std::process::exit(1);
        }
    };
    // Iced's boot callback is `Fn`, while the primary endpoint is consumed
    // exactly once. The option is only taken during daemon startup.
    let primary = Arc::new(Mutex::new(Some(primary)));
    // Daemon so we can open library + editor windows (same process / app_id).
    iced::daemon(
        move || {
            App::boot(
                primary
                    .lock()
                    .unwrap()
                    .take()
                    .expect("wallstudio boot once"),
            )
        },
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(App::theme)
    .subscription(App::subscription)
    .run()
}

/// Tab to open on, from the command line. Bindable to a hotkey — going
/// straight to Settings beats launching and clicking.
fn startup_tab() -> MainTab {
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "--settings" | "-s" => return MainTab::Settings,
            "--workshop" | "-w" => return MainTab::Workshop,
            "--library" | "-l" => return MainTab::Library,
            _ => {}
        }
    }
    MainTab::Library
}

fn window_settings(size: iced::Size) -> window::Settings {
    window::Settings {
        size,
        platform_specific: window::settings::PlatformSpecific {
            application_id: "wallstudio".into(),
            ..Default::default()
        },
        // Don't destroy the library window on X — we intercept the close
        // request and park it in the tray instead (Steam-style).
        exit_on_close_request: false,
        ..Default::default()
    }
}

/// Top-level library window tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MainTab {
    #[default]
    Library,
    Workshop,
    Settings,
}

/// A destructive action waiting for a second confirmation.
///
/// Unsubscribing wipes downloaded files and deleting removes a local project —
/// both are one keystroke away (`U`, and the Delete button), so by default
/// they arm first and fire on the second press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    /// Unsubscribe the selected library entry (workshop id, title).
    LibraryUnsubscribe(String, String),
    /// Unsubscribe the selected remote workshop item (workshop id, title).
    WorkshopUnsubscribe(String, String),
    /// Delete a local project (project dir, title).
    DeleteProject(std::path::PathBuf, String),
}

impl Confirm {
    fn prompt(&self) -> String {
        match self {
            Self::LibraryUnsubscribe(_, t) | Self::WorkshopUnsubscribe(_, t) => {
                format!("unsubscribe «{t}» and delete its files? press again to confirm")
            }
            Self::DeleteProject(_, t) => {
                format!("delete local project «{t}»? press again to confirm")
            }
        }
    }
}

/// Remote Steam Workshop browser state.
#[derive(Debug, Clone)]
pub struct WorkshopState {
    pub items: Vec<WorkshopItem>,
    pub cursor: usize,
    pub page: u32,
    pub sort: WorkshopSort,
    pub search: String,
    pub filter_type: TypeFilter,
    pub filter_genres: BTreeSet<String>,
    pub filter_ratings: BTreeSet<String>,
    pub loading: bool,
    pub error: Option<String>,
    /// True after the first successful load (avoids auto-refetch spam).
    pub loaded_once: bool,
}

impl Default for WorkshopState {
    fn default() -> Self {
        Self {
            items: Vec::new(),
            cursor: 0,
            page: 1,
            sort: WorkshopSort::Trend,
            search: String::new(),
            filter_type: TypeFilter::All,
            filter_genres: BTreeSet::new(),
            filter_ratings: BTreeSet::new(),
            loading: false,
            error: None,
            loaded_once: false,
        }
    }
}

impl WorkshopState {
    pub fn selected(&self) -> Option<&WorkshopItem> {
        self.items.get(self.cursor)
    }

    pub fn build_query(&self) -> WorkshopQuery {
        let mut tags = Vec::new();
        match self.filter_type {
            TypeFilter::All => {}
            TypeFilter::Scene => tags.push("Scene".into()),
            TypeFilter::Video => tags.push("Video".into()),
            TypeFilter::Web => tags.push("Web".into()),
        }
        for g in &self.filter_genres {
            tags.push(g.clone());
        }
        for r in &self.filter_ratings {
            tags.push(r.clone());
        }
        let sort = if !self.search.trim().is_empty() && matches!(self.sort, WorkshopSort::Trend) {
            WorkshopSort::TextSearch
        } else {
            self.sort
        };
        WorkshopQuery {
            sort,
            page: self.page.max(1),
            search: self.search.clone(),
            tags,
        }
    }
}

pub struct App {
    /// Keeps the Unix socket claimed until WallStudio exits.
    _single_instance: single_instance::Guard,
    /// Requests from later launcher invocations to raise this window.
    single_instance_rx: single_instance::Stream,
    pub library_id: Option<window::Id>,
    pub editors: HashMap<window::Id, EditorSession>,
    pub tab: MainTab,
    pub entries: Vec<WeEntry>,
    pub cursor: usize,
    pub filter: String,
    pub filter_type: TypeFilter,
    pub filter_source_workshop: bool,
    pub filter_source_local: bool,
    /// Selected WE genre tags (Anime, Game, Nature, …). Empty = all genres.
    /// Matched against each wallpaper's `project.json` `tags` array.
    pub filter_genres: BTreeSet<String>,
    /// Selected WE age ratings (Everyone / Mature / Questionable). Empty = all.
    /// Matched against `project.json` `contentrating`.
    pub filter_ratings: BTreeSet<String>,
    pub sort_newest: bool,
    pub monitors: Vec<MonitorInfo>,
    /// empty = all
    pub monitor: String,
    pub silent: bool,
    /// Preferred play engine (walld or linux-wallpaperengine).
    pub engine: PlayBackend,
    pub runtime: RuntimeStatus,
    pub last_msg: String,
    pub last_ok: bool,
    pub busy: bool,
    /// Logical window size — drives responsive gallery columns/tile size.
    pub window_width: f32,
    pub window_height: f32,
    /// Decoded frames for animated GIF previews.
    pub gifs: gifanim::GifCache,
    /// Editable properties for the selected wallpaper (WE settings).
    pub props: Vec<PropDef>,
    /// Workshop id the props list belongs to.
    pub props_id: String,
    /// Runtime presentation (pause, rate, fit, zoom, pan, flip).
    pub present: PresentSettings,
    /// Which color prop is expanded for the full picker (`None` = collapsed).
    pub color_open: Option<String>,
    /// Hex draft text for the open color picker.
    pub color_hex_draft: String,
    /// Draft title for renaming a local project.
    pub rename_draft: String,
    /// Shared keyboard modifiers (Ctrl/Shift for multi-select in editor).
    pub keyboard_mods: Modifiers,
    /// Remote Steam Workshop browser.
    pub workshop: WorkshopState,
    pub downloads: Vec<downloads::Download>,
    download_polling: bool,
    pub download_animation: f32,
    /// Last seen count of local workshop packages — used to auto-refresh the
    /// library when Steam finishes downloading a new subscription.
    pub workshop_pkg_count: usize,
    /// Top-bar monitor dropdown open.
    pub monitor_menu_open: bool,
    /// Play-button monitor dropdown open.
    pub play_monitor_menu_open: bool,
    /// Debounced hot-apply to walld (`path`, `key`, `value`). Full scene
    /// reloads are expensive — slider drags queue here and flush after idle.
    pending_prop: Option<(String, String, String)>,
    /// AnimTick counts since last prop change (~80ms each).
    pending_prop_idle: u32,
    /// Persistent preferences edited in the Settings tab.
    pub settings: Settings,
    /// Tab to return to when leaving Settings.
    pub prev_tab: MainTab,
    /// Draft text for the fixed-accent hex field (may be mid-edit).
    pub accent_hex_draft: String,
    /// Derived accent per wallpaper. Preview decoding is far too slow to redo
    /// on every arrow-key move, and the answer never changes for a given file.
    accent_cache: HashMap<String, Option<iced::Color>>,
    /// Cache key of the palette currently installed (skips redundant derives).
    palette_key: String,
    /// Destructive action armed and awaiting a second press.
    pub confirm: Option<Confirm>,
    /// Steam-style system tray (StatusNotifierItem). `None` when the desktop
    /// has no tray host — the app then closes to exit as before.
    tray: Option<ksni::blocking::Handle<TrayIcon>>,
    /// Receiver for tray menu events, fed to a subscription.
    tray_rx: TrayStream,
    /// Last state pushed to the tray, so we only touch D-Bus on change.
    tray_last: Option<TrayState>,
}

/// Wallpaper Engine–style layout / playback controls (sent to `walld we_present`).
#[derive(Debug, Clone)]
pub struct PresentSettings {
    pub paused: bool,
    pub rate: f32,
    pub mute: bool,
    pub fit: FitModeUi,
    pub zoom: f32,
    pub pos_x: f32,
    pub pos_y: f32,
    pub flip_h: bool,
    pub flip_v: bool,
}

impl Default for PresentSettings {
    fn default() -> Self {
        Self {
            paused: false,
            rate: 1.0,
            mute: true,
            fit: FitModeUi::Cover,
            zoom: 1.0,
            pos_x: 0.0,
            pos_y: 0.0,
            flip_h: false,
            flip_v: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FitModeUi {
    Cover,
    Contain,
    Fill,
}

impl FitModeUi {
    pub const ALL: [FitModeUi; 3] = [FitModeUi::Cover, FitModeUi::Contain, FitModeUi::Fill];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cover => "cover",
            Self::Contain => "contain",
            Self::Fill => "fill",
        }
    }

    /// Inverse of [`FitModeUi::as_str`]; anything unrecognised is `Cover`.
    pub fn parse(s: &str) -> Self {
        match s {
            "contain" => Self::Contain,
            "fill" => Self::Fill,
            _ => Self::Cover,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Cover => "Cover (crop)",
            Self::Contain => "Contain (letterbox)",
            Self::Fill => "Fill (stretch)",
        }
    }
}

impl std::fmt::Display for FitModeUi {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFilter {
    All,
    Scene,
    Video,
    Web,
}

/// Official Wallpaper Engine genre / content tags (browser filter list).
pub const WE_GENRE_TAGS: &[&str] = &[
    "Abstract",
    "Animal",
    "Anime",
    "Cartoon",
    "CGI",
    "Cyberpunk",
    "Fantasy",
    "Game",
    "Girls",
    "Guys",
    "Landscape",
    "Medieval",
    "Memes",
    "MMD",
    "Music",
    "Nature",
    "Pixel art",
    "Relaxing",
    "Retro",
    "Sci-Fi",
    "Sports",
    "Technology",
    "Television",
    "Unspecified",
    "Vehicle",
];

/// Official Wallpaper Engine age / content ratings (`contentrating` field).
pub const WE_AGE_RATINGS: &[&str] = &["Everyone", "Questionable", "Mature"];

/// Debounce granularity for queued property applies (two ticks ≈ 240ms).
const PROP_FLUSH_TICK_MS: u64 = 120;

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Tick,
    /// Advance animated GIF previews.
    AnimTick,
    /// Debounce timer for queued property applies.
    PropFlush,
    FilterChanged(String),
    Select(usize),
    Move(i32, i32),
    Apply,
    /// Play selected wallpaper on a specific monitor (empty string = all).
    ApplyOnMonitor(String),
    Stop,
    SetMonitor(String),
    ToggleMonitorMenu,
    TogglePlayMonitorMenu,
    /// Cycle preferred engine walld ↔ LWE.
    ToggleEngine,
    SetTypeFilter(TypeFilter),
    ToggleWorkshop,
    ToggleLocal,
    /// Toggle a WE genre tag in the library filter.
    ToggleGenre(String),
    /// Clear all genre selections (show every genre).
    ClearGenres,
    /// Toggle a WE age rating (Everyone / Mature / Questionable).
    ToggleRating(String),
    /// Clear age-rating filter (show every rating).
    ClearRatings,
    ToggleSilent,
    ToggleSort,
    OpenFolder,
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
    /// Library window resized (editor resizes must not affect gallery grid).
    Resized(window::Id, f32, f32),
    /// Property edits
    PropBool(String, bool),
    PropSlider(String, f64),
    PropText(String, String),
    PropCombo(String, String),
    PropColor(String, f32, f32, f32),
    PropColorHex(String, String),
    ToggleColorOpen(String),
    PropReset,
    /// Presentation
    PresentPause(bool),
    PresentRate(f32),
    PresentMute(bool),
    PresentFit(FitModeUi),
    PresentZoom(f32),
    PresentPosX(f32),
    PresentPosY(f32),
    PresentFlipH(bool),
    PresentFlipV(bool),
    PresentReset,
    /// Fork selected workshop scene and open editor window.
    EditScene,
    /// Open editor on an existing local project (already forked).
    EditExisting,
    Editor(window::Id, EditorMessage),
    WindowClosed(window::Id),
    /// The user asked to close the library window — park the app in the tray
    /// instead of quitting (Steam-style).
    CloseRequested(window::Id),
    /// A right-click tray menu action.
    Tray(TrayEvent),
    /// A later `wallstudio` invocation asked this process to show itself.
    Raise,
    /// Local project management
    RenameDraft(String),
    RenameProject,
    DeleteProject,
    /// Always track modifier keys (for editor multi-select).
    ModifiersChanged(Modifiers),
    /// Switch Library / Workshop tab.
    SetTab(MainTab),
    /// Remote workshop browser
    WorkshopSearch(String),
    WorkshopSetSort(WorkshopSort),
    WorkshopSetType(TypeFilter),
    WorkshopToggleGenre(String),
    WorkshopClearGenres,
    WorkshopToggleRating(String),
    WorkshopClearRatings,
    WorkshopSelect(usize),
    WorkshopMove(i32, i32),
    WorkshopPage(i32),
    WorkshopRefresh,
    WorkshopLoaded(Result<BrowsePage, String>),
    /// One preview finished downloading (or failed) — paint as it arrives.
    WorkshopPreviewReady(String, Option<std::path::PathBuf>),
    WorkshopSubscribe,
    WorkshopSubscribed(String, Result<(), String>),
    DownloadsPolled(downloads::Snapshot),
    ShowDownload(String),
    WorkshopUnsubscribe,
    WorkshopOpenSteam,
    WorkshopOpenWeb,
    WorkshopPlayInstalled,
    /// Library: unsubscribe selected Steam workshop wallpaper (opens Steam + removes local).
    LibraryUnsubscribe,
    /// Settings tab edits.
    Settings(SettingsMessage),
    /// Dismiss an armed destructive action.
    CancelConfirm,
}

/// One control in the Settings tab.
#[derive(Debug, Clone)]
pub enum SettingsMessage {
    // Appearance
    DynamicAccent(bool),
    AccentSource(AccentSource),
    AccentFollow(AccentFollow),
    Tint(f32),
    Gradients(bool),
    Radius(f32),
    /// Typing in the fixed-accent hex field.
    FixedAccentHex(String),
    /// Freeze the wallpaper's current accent as the fixed one.
    PinCurrentAccent,
    // Quality
    SceneFps(u32),
    VideoCap(VideoCap),
    LweFps(u32),
    PreviewAnim(PreviewAnim),
    PreviewFps(u32),
    TileSize(u32),
    // Behaviour
    ConfirmDestructive(bool),
    DesktopAccent(bool),
    /// Fade recolors instead of snapping; mirrored into wallaccent's config.
    SmoothTransition(bool),
    /// Re-run wallaccent right now.
    RecolorDesktop,
    AutoRefresh(bool),
    DefaultFit(FitModeUi),
    // Actions
    ResetAppearance,
    ResetQuality,
    ResetAll,
    OpenConfigDir,
    /// Drop cached preview-derived accents and re-theme.
    RecomputeAccents,
}

impl App {
    fn boot(primary: single_instance::Primary) -> (Self, Task<Message>) {
        let prefs = load_session_prefs();
        let cfg = settings::load();
        let (library_id, open) = window::open(window_settings(iced::Size::new(1280.0, 800.0)));
        let tab = startup_tab();
        let (tray_tx, tray_rx) = iced::futures::channel::mpsc::unbounded();
        let tray = tray::spawn(tray_tx);
        let mut app = Self {
            _single_instance: primary.guard,
            single_instance_rx: primary.stream,
            library_id: Some(library_id),
            editors: HashMap::new(),
            tab,
            entries: Vec::new(),
            cursor: 0,
            filter: prefs.filter.clone(),
            filter_type: prefs.filter_type,
            filter_source_workshop: prefs.filter_source_workshop,
            filter_source_local: prefs.filter_source_local,
            filter_genres: prefs.filter_genres.clone(),
            filter_ratings: prefs.filter_ratings.clone(),
            sort_newest: prefs.sort_newest,
            monitors: discover_monitors_info(),
            monitor: prefs.monitor.clone(),
            silent: prefs.silent,
            engine: prefs.engine,
            runtime: status_snapshot(),
            last_msg: "Wallpaper Engine library · select a tile · Enter to play".into(),
            last_ok: true,
            busy: false,
            window_width: 1280.0,
            window_height: 800.0,
            gifs: gifanim::GifCache::default(),
            props: Vec::new(),
            props_id: String::new(),
            present: PresentSettings::default(),
            color_open: None,
            color_hex_draft: String::new(),
            rename_draft: String::new(),
            keyboard_mods: Modifiers::default(),
            workshop: WorkshopState::default(),
            downloads: downloads::load(),
            download_polling: false,
            download_animation: 0.0,
            workshop_pkg_count: workshop::count_local_workshop_packages(),
            monitor_menu_open: false,
            play_monitor_menu_open: false,
            pending_prop: None,
            pending_prop_idle: 0,
            accent_hex_draft: theme::to_hex(cfg.appearance.fixed_accent),
            settings: cfg,
            prev_tab: if tab == MainTab::Settings {
                MainTab::Library
            } else {
                tab
            },
            accent_cache: HashMap::new(),
            palette_key: String::new(),
            confirm: None,
            tray,
            tray_rx: TrayStream::new(tray_rx),
            tray_last: None,
        };
        app.reload();
        app.reload_props();
        // Paint the very first frame with the right theme, not the default gold.
        app.refresh_palette(true);
        if tab == MainTab::Workshop {
            let fetch = app.workshop_fetch_task();
            return (app, Task::batch([open.map(|_| Message::Tick), fetch]));
        }
        (app, open.map(|_| Message::Tick))
    }

    /// Persist filter / engine / monitor choices for the next session.
    pub fn save_session(&self) {
        save_session_prefs(&SessionPrefs {
            filter: self.filter.clone(),
            filter_type: self.filter_type,
            filter_source_workshop: self.filter_source_workshop,
            filter_source_local: self.filter_source_local,
            filter_genres: self.filter_genres.clone(),
            filter_ratings: self.filter_ratings.clone(),
            sort_newest: self.sort_newest,
            monitor: self.monitor.clone(),
            silent: self.silent,
            engine: self.engine,
        });
    }

    /// Image handle for a preview, honouring the preview-animation setting.
    /// `focused` marks the places that keep animating in "selected only" mode:
    /// the selected tile, the detail panel and the display picker.
    pub fn preview_handle(
        &self,
        path: &std::path::Path,
        focused: bool,
    ) -> iced::widget::image::Handle {
        let animate = match self.settings.quality.preview_anim {
            PreviewAnim::All => true,
            PreviewAnim::Selected => focused,
            PreviewAnim::Off => false,
        };
        if animate {
            if let Some(h) = self.gifs.handle(path) {
                return h;
            }
        }
        iced::widget::image::Handle::from_path(path.to_path_buf())
    }

    /// Untouched presentation state, using the configured default fit.
    fn default_present(&self) -> PresentSettings {
        PresentSettings {
            fit: self.settings.behaviour.default_fit,
            ..PresentSettings::default()
        }
    }

    /// Ask `wallaccent` to regenerate the desktop palette from `wallpaper`
    /// (or from whatever is live when `None`).
    ///
    /// Detached and best-effort: decoding a preview can take a moment and a
    /// missing `wallaccent` is not an error — it's an optional companion.
    fn push_desktop_accent(&mut self, wallpaper: Option<&std::path::Path>) {
        if !self.settings.behaviour.desktop_accent {
            return;
        }
        let Some(bin) = we::wallaccent_binary() else {
            self.last_msg = "wallaccent not installed — desktop palette skipped".into();
            self.last_ok = false;
            return;
        };
        let mut cmd = std::process::Command::new(&bin);
        cmd.arg("apply").arg("--quiet");
        if let Some(p) = wallpaper {
            cmd.arg("--wallpaper").arg(p);
        }
        match cmd
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Don't leave a zombie behind; don't block the UI either.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => {
                self.last_msg = format!("wallaccent: {e}");
                self.last_ok = false;
            }
        }
    }

    /// Persist the Settings tab to disk and surface any write failure.
    fn save_settings(&mut self) {
        if let Err(e) = settings::save(&self.settings) {
            self.last_msg = format!("settings not saved — {e}");
            self.last_ok = false;
        }
    }

    /// Apply a settings edit: persist, re-theme, and report.
    fn settings_changed(&mut self, note: impl Into<String>) {
        self.save_settings();
        self.refresh_palette(true);
        if self.last_ok {
            self.last_msg = note.into();
        }
    }

    /// Gate a destructive action behind a second press.
    ///
    /// Returns `true` when the caller should go ahead: either confirmation is
    /// switched off, or this exact action was already armed.
    fn arm(&mut self, action: Confirm) -> bool {
        if !self.settings.behaviour.confirm_destructive {
            self.confirm = None;
            return true;
        }
        if self.confirm.as_ref() == Some(&action) {
            self.confirm = None;
            return true;
        }
        self.last_msg = action.prompt();
        self.last_ok = false;
        self.confirm = Some(action);
        false
    }

    /// True when `action` is armed and its button should read "confirm".
    pub fn is_armed(&self, action: &Confirm) -> bool {
        self.confirm.as_ref() == Some(action)
    }

    fn update_settings(&mut self, msg: SettingsMessage) -> Task<Message> {
        use SettingsMessage as S;
        match msg {
            S::DynamicAccent(v) => {
                self.settings.appearance.dynamic_accent = v;
                self.settings_changed(if v {
                    "theme follows the wallpaper"
                } else {
                    "theme pinned to the fixed accent"
                });
            }
            S::AccentSource(v) => {
                self.settings.appearance.source = v;
                self.settings_changed(format!("accent source · {}", v.label()));
            }
            S::AccentFollow(v) => {
                self.settings.appearance.follow = v;
                self.settings_changed(format!("accent follows · {}", v.label()));
            }
            S::Tint(v) => {
                self.settings.appearance.tint = v.clamp(0.0, 1.0);
                self.settings_changed(format!("tint · {:.0}%", v * 100.0));
            }
            S::Gradients(v) => {
                self.settings.appearance.gradients = v;
                self.settings_changed(if v { "gradients on" } else { "gradients off" });
            }
            S::Radius(v) => {
                self.settings.appearance.radius = v.clamp(0.0, 16.0);
                self.settings_changed(format!("corner radius · {v:.0}px"));
            }
            S::FixedAccentHex(hex) => {
                self.accent_hex_draft = hex.clone();
                // Typing "#3" shouldn't reset the colour — only commit on a
                // complete, parsable value.
                if let Some(c) = theme::from_hex(&hex) {
                    self.settings.appearance.fixed_accent = c;
                    self.settings_changed(format!("fixed accent · {}", theme::to_hex(c)));
                }
            }
            S::PinCurrentAccent => {
                let current = theme::active().accent;
                self.settings.appearance.fixed_accent = current;
                self.settings.appearance.dynamic_accent = false;
                self.accent_hex_draft = theme::to_hex(current);
                self.settings_changed(format!("pinned {} as the theme", theme::to_hex(current)));
            }
            S::SceneFps(v) => {
                self.settings.quality.scene_fps = v.clamp(5, 120);
                self.save_settings();
                self.push_quality_to_walld();
            }
            S::VideoCap(v) => {
                self.settings.quality.video_cap = v;
                self.save_settings();
                self.push_quality_to_walld();
            }
            S::LweFps(v) => {
                self.settings.quality.lwe_fps = v.clamp(5, 144);
                self.settings_changed(format!(
                    "LWE frame cap · {} fps (applies on next Play)",
                    self.settings.quality.lwe_fps
                ));
            }
            S::PreviewAnim(v) => {
                self.settings.quality.preview_anim = v;
                self.settings_changed(format!("preview animation · {}", v.label()));
            }
            S::PreviewFps(v) => {
                self.settings.quality.preview_fps = v.clamp(1, 60);
                self.settings_changed(format!(
                    "preview animation · {} fps",
                    self.settings.quality.preview_fps
                ));
            }
            S::TileSize(v) => {
                self.settings.quality.tile_size = v.clamp(140, 400);
                self.settings_changed(format!("tile size · {}px", self.settings.quality.tile_size));
            }
            S::ConfirmDestructive(v) => {
                self.settings.behaviour.confirm_destructive = v;
                if !v {
                    self.confirm = None;
                }
                self.settings_changed(if v {
                    "unsubscribe & delete now ask twice"
                } else {
                    "unsubscribe & delete fire immediately"
                });
            }
            S::DesktopAccent(v) => {
                self.settings.behaviour.desktop_accent = v;
                self.settings_changed(if v {
                    "desktop palette follows the wallpaper you play"
                } else {
                    "desktop palette updates paused"
                });
                if v {
                    self.push_desktop_accent(None);
                }
            }
            S::SmoothTransition(v) => {
                self.settings.behaviour.smooth_transition = v;
                self.settings_changed(if v {
                    "desktop recolors fade to the next accent"
                } else {
                    "desktop recolors snap to the next accent"
                });
                // Mirror into wallaccent's own config so walld-triggered
                // recolors fade too, not just the play-from-UI ones.
                let out = we::wallaccent_binary().map(|bin| {
                    std::process::Command::new(bin)
                        .args(["smooth", if v { "on" } else { "off" }])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                });
                match out {
                    Some(Ok(s)) if s.success() => {}
                    _ => {
                        self.last_msg =
                            "saved here, but `wallaccent smooth` failed — recolors from \
                                         wallpaper switches won't follow"
                                .into();
                        self.last_ok = false;
                    }
                }
            }
            S::RecolorDesktop => {
                // Explicit request: run it even when the setting is off.
                let was = self.settings.behaviour.desktop_accent;
                self.settings.behaviour.desktop_accent = true;
                self.push_desktop_accent(None);
                self.settings.behaviour.desktop_accent = was;
                if self.last_ok {
                    self.last_msg = "recoloring the desktop from the live wallpaper…".into();
                }
            }
            S::AutoRefresh(v) => {
                self.settings.behaviour.auto_refresh = v;
                self.settings_changed(if v {
                    "library auto-refreshes on Steam downloads"
                } else {
                    "auto-refresh off · use Refresh (R)"
                });
            }
            S::DefaultFit(v) => {
                self.settings.behaviour.default_fit = v;
                self.settings_changed(format!("default fit · {}", v.label()));
            }
            S::ResetAppearance => {
                self.settings.appearance = settings::Appearance::default();
                self.accent_hex_draft = theme::to_hex(self.settings.appearance.fixed_accent);
                self.settings_changed("appearance reset to defaults");
            }
            S::ResetQuality => {
                self.settings.quality = settings::Quality::default();
                self.save_settings();
                self.push_quality_to_walld();
            }
            S::ResetAll => {
                self.settings = Settings::default();
                self.accent_hex_draft = theme::to_hex(self.settings.appearance.fixed_accent);
                self.accent_cache.clear();
                self.save_settings();
                self.refresh_palette(true);
                self.push_quality_to_walld();
                if self.last_ok {
                    self.last_msg = "all settings reset to defaults".into();
                }
            }
            S::RecomputeAccents => {
                self.accent_cache.clear();
                self.refresh_palette(true);
                self.last_msg = "re-read wallpaper colors".into();
                self.last_ok = true;
            }
            S::OpenConfigDir => {
                let dir = settings::config_dir();
                let _ = std::fs::create_dir_all(&dir);
                let _ = std::process::Command::new("xdg-open").arg(&dir).spawn();
                self.last_msg = format!("opened {}", dir.display());
                self.last_ok = true;
            }
        }
        Task::none()
    }

    /// Write walld's own render options and ask a running daemon to re-read
    /// them. Unlike `walld ctl reload`, `cfg_reload` doesn't re-apply
    /// wallpapers, so changing quality never disturbs what's on screen.
    fn push_quality_to_walld(&mut self) {
        let q = &self.settings.quality;
        let fps = q.scene_fps;
        let cap = q.video_cap;
        if let Err(e) = write_walld_options(&[
            ("scene_fps", fps.to_string()),
            ("video_max_edge", cap.max_edge().to_string()),
        ]) {
            self.last_msg = format!("walld config not written — {e}");
            self.last_ok = false;
            return;
        }
        let bin = we::walld_binary();
        let out = std::process::Command::new(&bin)
            .args(["ctl", "cfg_reload"])
            .output();
        let live = matches!(&out, Ok(o) if o.status.success());
        self.last_msg = if live {
            format!("quality · {fps} fps · {} (live)", cap.label())
        } else {
            format!(
                "quality · {fps} fps · {} (saved — applies when walld restarts)",
                cap.label()
            )
        };
        self.last_ok = true;
    }

    /// The wallpaper the accent should follow: (cache key, declared scheme
    /// colour, preview image). Workshop browsing themes off the item you're
    /// looking at even though it isn't installed.
    fn accent_subject(&self) -> Option<(String, Option<iced::Color>, Option<std::path::PathBuf>)> {
        if self.tab == MainTab::Workshop {
            let item = self.workshop.selected()?;
            return Some((format!("ws:{}", item.id), None, item.preview_path.clone()));
        }
        let e = match self.settings.appearance.follow {
            AccentFollow::Playing => self
                .entry_on_monitor(&self.monitor)
                .or_else(|| self.selected())?,
            AccentFollow::Selected => self.selected()?,
        };
        Some((
            format!("we:{}", e.id),
            self.scheme_color_of(e),
            e.preview.clone(),
        ))
    }

    /// Title of the wallpaper the accent came from (for the settings preview).
    pub fn accent_subject_label(&self) -> Option<String> {
        if self.tab == MainTab::Workshop {
            return self.workshop.selected().map(|i| i.title.clone());
        }
        let e = match self.settings.appearance.follow {
            AccentFollow::Playing => self
                .entry_on_monitor(&self.monitor)
                .or_else(|| self.selected())?,
            AccentFollow::Selected => self.selected()?,
        };
        Some(e.project.title.clone())
    }

    /// A wallpaper's WE scheme colour, preferring a live user override so the
    /// chrome tracks the schemecolor slider as it's dragged.
    fn scheme_color_of(&self, e: &WeEntry) -> Option<iced::Color> {
        if e.id == self.props_id {
            if let Some(PropValue::Color([r, g, b])) = self
                .props
                .iter()
                .find(|p| p.key == "schemecolor")
                .map(|p| p.value.clone())
            {
                // A wallpaper that ships plain white/black declared no accent.
                let bright = r >= 0.999 && g >= 0.999 && b >= 0.999;
                let dark = r <= 0.001 && g <= 0.001 && b <= 0.001;
                if !bright && !dark {
                    return Some(iced::Color::from_rgb(r, g, b));
                }
                return None;
            }
        }
        e.project
            .scheme_color()
            .map(|[r, g, b]| iced::Color::from_rgb(r, g, b))
    }

    /// Accent for the current subject, honouring the configured source.
    /// Preview decoding is memoised — it's far too slow to redo per keystroke.
    fn derive_accent(
        &mut self,
        key: &str,
        scheme: Option<iced::Color>,
        preview: Option<std::path::PathBuf>,
    ) -> Option<iced::Color> {
        let source = self.settings.appearance.source;
        if matches!(source, AccentSource::Auto | AccentSource::Scheme) {
            if let Some(c) = scheme {
                return Some(c);
            }
        }
        if matches!(source, AccentSource::Auto | AccentSource::Preview) {
            let p = preview?;
            if let Some(hit) = self.accent_cache.get(key) {
                return *hit;
            }
            let derived =
                wallengine_we::dominant_color(&p).map(|[r, g, b]| iced::Color::from_rgb(r, g, b));
            self.accent_cache.insert(key.to_string(), derived);
            return derived;
        }
        None
    }

    /// Recompute and install the chrome palette. `force` re-derives even when
    /// the same wallpaper is still selected (after a settings change).
    fn refresh_palette(&mut self, force: bool) {
        let subject = self.accent_subject();
        let key = match &subject {
            Some((k, ..)) => k.clone(),
            None => String::new(),
        };
        // Settings edits change the palette without changing the wallpaper.
        let full_key = format!("{key}|{:?}", self.settings.appearance);
        if !force && full_key == self.palette_key {
            return;
        }
        self.palette_key = full_key;
        let accent = match subject {
            Some((k, scheme, preview)) if self.settings.appearance.dynamic_accent => {
                self.derive_accent(&k, scheme, preview)
            }
            _ => None,
        };
        theme::set_active(self.settings.palette_for(accent));
    }

    /// Entry currently playing on `monitor` (empty = first active / any).
    pub fn entry_on_monitor(&self, monitor: &str) -> Option<&WeEntry> {
        let id = monitor_wallpaper_id(&self.runtime, monitor)?;
        self.entries.iter().find(|e| e.id == id)
    }

    /// Kick off a background Steam workshop browse (metadata only — fast).
    fn workshop_fetch_task(&mut self) -> Task<Message> {
        let query = self.workshop.build_query();
        self.workshop.loading = true;
        self.workshop.error = None;
        // Clear immediately so the UI paints a loading state instead of
        // freezing on the previous page for several seconds.
        self.workshop.items.clear();
        self.workshop.cursor = 0;
        self.last_msg = format!("workshop · loading page {}…", query.page);
        self.last_ok = true;
        Task::perform(
            async move {
                match std::thread::spawn(move || workshop::browse(query)).join() {
                    Ok(r) => r,
                    Err(_) => Err("workshop browse worker panicked".into()),
                }
            },
            Message::WorkshopLoaded,
        )
    }

    fn apply_workshop_page(&mut self, page: BrowsePage) -> Task<Message> {
        self.workshop.page = page.page;
        self.workshop.items = page.items;
        self.workshop.cursor = 0;
        self.workshop.loading = false;
        self.workshop.error = None;
        self.workshop.loaded_once = true;
        self.last_msg = format!(
            "workshop · page {} · {} items",
            self.workshop.page,
            self.workshop.items.len()
        );
        self.last_ok = true;
        // Fill missing previews in the background so tiles appear progressively.
        self.workshop_preview_tasks()
    }

    /// Spawn one background download per missing preview URL.
    fn workshop_preview_tasks(&self) -> Task<Message> {
        let jobs: Vec<(String, String)> = self
            .workshop
            .items
            .iter()
            .filter(|i| i.preview_path.is_none() && !i.preview_url.is_empty())
            .map(|i| (i.id.clone(), i.preview_url.clone()))
            .collect();
        if jobs.is_empty() {
            return Task::none();
        }
        Task::batch(jobs.into_iter().map(|(id, url)| {
            let id_msg = id.clone();
            Task::perform(
                async move {
                    std::thread::spawn(move || workshop::ensure_preview(&id, &url))
                        .join()
                        .ok()
                        .and_then(|r| r.ok())
                },
                move |path| Message::WorkshopPreviewReady(id_msg, path),
            )
        }))
    }

    /// Silent account unsubscribe + wipe local workshop files for `id`.
    fn unsubscribe_workshop_id(&mut self, id: &str, title: &str) {
        if self
            .downloads
            .iter()
            .any(|d| d.item.id == id && d.state == downloads::State::Subscribing)
        {
            self.last_msg = "Wait for the subscription request to finish".into();
            return;
        }
        // If this wallpaper is currently on the desktop, stop it first.
        if self.runtime.playing && (self.runtime.title == id || self.runtime.detail.contains(id)) {
            stop_all();
            self.runtime = status_snapshot();
        }
        match workshop::unsubscribe(id) {
            Ok(acknowledged) => {
                self.downloads.retain(|d| d.item.id != id);
                downloads::save(&self.downloads);
                // Only sweep leftovers once Steam has dropped its install
                // record; deleting sooner blocks any future re-download.
                let removed = acknowledged && workshop::remove_local(id).unwrap_or(false);
                self.workshop_pkg_count = workshop::count_local_workshop_packages();
                self.reload();
                self.last_msg = if removed || acknowledged {
                    format!("unsubscribed «{title}» · local files removed")
                } else {
                    format!("unsubscribed «{title}» · Steam will remove the files shortly")
                };
                self.last_ok = true;
            }
            Err(e) => {
                self.last_msg = e;
                self.last_ok = false;
            }
        }
    }

    fn subscribe_workshop_item(&mut self, item: WorkshopItem) -> Task<Message> {
        if self
            .downloads
            .iter()
            .any(|d| d.item.id == item.id && !matches!(d.state, downloads::State::Failed(_)))
        {
            return Task::none();
        }
        self.downloads.retain(|d| d.item.id != item.id);
        let id = item.id.clone();
        let failed_id = id.clone();
        self.last_msg = format!("subscribing «{}»…", item.title);
        self.downloads.insert(
            0,
            downloads::Download {
                item,
                state: downloads::State::Subscribing,
            },
        );
        downloads::save(&self.downloads);
        // The network request must not block the first paint of the pending tile.
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let result = workshop::subscribe(&id);
            let _ = tx.send((id, result));
        });
        Task::perform(async move { rx.await }, move |result| match result {
            Ok((id, result)) => Message::WorkshopSubscribed(id, result),
            Err(_) => Message::WorkshopSubscribed(
                failed_id.clone(),
                Err("Subscription request was interrupted. Retry to continue.".into()),
            ),
        })
    }

    fn poll_downloads(&mut self) -> Task<Message> {
        if self.download_polling {
            return Task::none();
        }
        let ids: Vec<_> = self
            .downloads
            .iter()
            .filter(|d| d.state != downloads::State::Subscribing)
            .map(|d| d.item.id.clone())
            .collect();
        if ids.is_empty() {
            return Task::none();
        }
        self.download_polling = true;
        let (tx, rx) = iced::futures::channel::oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(downloads::poll(ids));
        });
        Task::perform(
            async move {
                rx.await
                    .unwrap_or(downloads::Snapshot { states: Vec::new() })
            },
            Message::DownloadsPolled,
        )
    }

    fn play_workshop_installed(&mut self, id: &str) {
        if self.downloads.iter().any(|d| d.item.id == id) {
            return;
        }
        let dir = we::workshop_dir().join(id);
        if !dir.join("project.json").is_file() {
            self.last_msg =
                "not downloaded yet — Subscribe in Steam, wait for download, then Refresh".into();
            self.last_ok = false;
            return;
        }
        // Prefer the library entry if present so type/backend detection matches.
        if let Some(idx) = self.entries.iter().position(|e| e.id == id) {
            self.cursor = idx;
            self.reload_props();
            self.apply_selected();
            return;
        }
        // Fallback: construct a minimal play request from disk.
        let pj = dir.join("project.json");
        let project = match wallengine_we::Project::load(&pj) {
            Ok(p) => p,
            Err(e) => {
                self.last_msg = format!("load project.json: {e}");
                self.last_ok = false;
                return;
            }
        };
        self.play_entry_dir(
            dir,
            id.to_string(),
            project.wallpaper_type,
            project.title,
            None,
        );
    }

    fn play_entry_dir(
        &mut self,
        dir: std::path::PathBuf,
        id: String,
        wallpaper_type: WallpaperType,
        title: String,
        monitor_override: Option<String>,
    ) {
        self.busy = true;
        self.play_monitor_menu_open = false;
        self.monitor_menu_open = false;
        // "Play" is an explicit request to run the wallpaper.  Presentation
        // settings are saved per wallpaper, so carrying a previously saved
        // paused state into this path made a successful play command install a
        // black, never-started video with no visible error.  Keep the other
        // presentation preferences, but always resume when the user presses
        // Play (Pause remains available as an intentional separate action).
        self.present.paused = false;
        let mon = monitor_override.unwrap_or_else(|| self.monitor.clone());
        let monitors = if mon.is_empty() {
            Vec::new()
        } else {
            vec![mon]
        };
        let backend = match self.engine {
            PlayBackend::Lwe => PlayBackend::Lwe,
            _ => PlayBackend::Walld,
        };
        let dir_for_accent = dir.clone();
        let req = PlayRequest {
            wallpaper_dir: dir,
            workshop_id: id,
            wallpaper_type,
            monitors,
            silent: self.silent,
            fps: self.settings.quality.lwe_fps,
            backend,
        };
        match we_play(&req) {
            Ok(st) => {
                self.runtime = st;
                if backend == PlayBackend::Walld {
                    self.sync_all_present();
                }
                self.last_msg = format!("playing «{title}» via {}", self.runtime.backend.label());
                self.last_ok = true;
            }
            Err(err) => {
                self.last_msg = err.to_string();
                self.last_ok = false;
                self.runtime = status_snapshot();
            }
        }
        self.busy = false;
        // "Follow: playing wallpaper" should re-theme the moment it changes.
        self.refresh_palette(false);
        if self.last_ok {
            self.push_desktop_accent(Some(&dir_for_accent));
        }
    }

    pub fn reload_props(&mut self) {
        let Some(e) = self.selected().cloned() else {
            self.props.clear();
            self.props_id.clear();
            self.present = self.default_present();
            self.color_open = None;
            self.rename_draft.clear();
            self.refresh_palette(false);
            return;
        };
        // Always re-read so overrides / new selection stay in sync.
        self.props_id = e.id.clone();
        self.props = list_props(&e.dir, &e.id);
        self.present = load_present_for(&e.id, &self.monitor, self.settings.behaviour.default_fit);
        self.silent = self.present.mute;
        self.color_open = None;
        self.rename_draft = e.project.title.clone();
        self.refresh_palette(false);
        log::debug!(
            "props for «{}» ({}): {} item(s)",
            e.project.title,
            e.id,
            self.props.len()
        );
    }

    fn push_present(&mut self, key: &str, value: &str) {
        let bin = we::walld_binary();
        // Scope present controls to the selected display so DP-1 zoom/mute
        // never touches the independent WE slot on DP-2.
        let out = if self.monitor.is_empty() {
            std::process::Command::new(&bin)
                .args(["ctl", "we_present", key, value])
                .output()
        } else {
            std::process::Command::new(&bin)
                .args(["ctl", "we_present", &self.monitor, key, value])
                .output()
        };
        match out {
            Ok(o) if o.status.success() => {
                self.last_msg = format!("present {key}={value}");
                self.last_ok = true;
            }
            Ok(o) => {
                let err = String::from_utf8_lossy(&o.stdout);
                let err2 = String::from_utf8_lossy(&o.stderr);
                self.last_msg = if !err.trim().is_empty() {
                    err.trim().to_string()
                } else {
                    err2.trim().to_string()
                };
                self.last_ok = false;
            }
            Err(e) => {
                self.last_msg = e.to_string();
                self.last_ok = false;
            }
        }
        // Keep local silent flag aligned with mute.
        if key == "mute" {
            self.silent = self.present.mute;
        }
        self.runtime = status_snapshot();
    }

    /// Apply a user property. `debounce` batches walld reloads (use for
    /// sliders/color channels so we don't full-reload the scene every tick).
    fn apply_prop(&mut self, key: &str, value: serde_json::Value, debounce: bool) {
        let Some(e) = self.selected().cloned() else {
            self.last_msg = "nothing selected".into();
            self.last_ok = false;
            return;
        };
        match set_override(&e.id, key, value.clone()) {
            Ok(()) => {
                // Update local UI state immediately.
                if let Some(p) = self.props.iter_mut().find(|p| p.key == key) {
                    p.value = match &value {
                        serde_json::Value::Bool(b) => PropValue::Bool(*b),
                        serde_json::Value::Number(n) => {
                            PropValue::Number(n.as_f64().unwrap_or(0.0))
                        }
                        serde_json::Value::String(s) => {
                            if matches!(p.kind, PropKind::Color) {
                                let parts: Vec<f32> = s
                                    .split_whitespace()
                                    .filter_map(|x| x.parse().ok())
                                    .collect();
                                PropValue::Color([
                                    parts.first().copied().unwrap_or(0.0),
                                    parts.get(1).copied().unwrap_or(0.0),
                                    parts.get(2).copied().unwrap_or(0.0),
                                ])
                            } else {
                                PropValue::Text(s.clone())
                            }
                        }
                        _ => PropValue::Text(value.to_string()),
                    };
                }
                let path = e.dir.display().to_string();
                let val_str = match &value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                };
                if debounce {
                    // Queue walld apply; flushed after a short idle.
                    self.pending_prop = Some((path, key.to_string(), val_str.clone()));
                    self.pending_prop_idle = 0;
                    self.last_msg = format!("set {key} = {val_str} · applying…");
                    self.last_ok = true;
                } else {
                    self.pending_prop = None;
                    self.flush_prop_to_walld(&path, key, &val_str);
                }
            }
            Err(e) => {
                self.last_msg = e;
                self.last_ok = false;
            }
        }
    }

    fn flush_prop_to_walld(&mut self, path: &str, key: &str, val_str: &str) {
        let bin = we::walld_binary();
        let out = std::process::Command::new(&bin)
            .args(["ctl", "we_set_prop", path, key, val_str])
            .output();
        match out {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&o.stderr).trim().to_string();
                let msg = if !stdout.is_empty() {
                    stdout
                } else if !stderr.is_empty() {
                    stderr
                } else if o.status.success() {
                    format!("set {key} = {val_str}")
                } else {
                    format!("we_set_prop failed for {key}")
                };
                let ok = o.status.success() && !msg.starts_with("err");
                // Clarify when override was saved but wallpaper isn't the active one.
                if ok && !msg.contains("reload") {
                    self.last_msg = format!(
                        "set {key} = {val_str} · saved (not live — hit Play if you don't see it)"
                    );
                } else {
                    self.last_msg = msg;
                }
                self.last_ok = ok;
            }
            Err(e) => {
                self.last_msg = format!("walld ctl: {e}");
                self.last_ok = false;
            }
        }
        self.runtime = status_snapshot();
    }

    fn flush_pending_prop(&mut self) {
        if let Some((path, key, val)) = self.pending_prop.take() {
            self.flush_prop_to_walld(&path, &key, &val);
        }
        self.pending_prop_idle = 0;
    }

    fn title(&self, id: window::Id) -> String {
        if let Some(ed) = self.editors.get(&id) {
            return ed.title();
        }
        "wallstudio — Wallpaper Engine".into()
    }

    fn theme(&self, _id: window::Id) -> Theme {
        Theme::Dark
    }

    fn subscription(&self) -> Subscription<Message> {
        use iced::event;
        use iced::keyboard;
        let mut subs = vec![
            iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick),
            event::listen_with(|event, status, id| match event {
                iced::Event::Window(window::Event::Resized(size)) => {
                    Some(Message::Resized(id, size.width, size.height))
                }
                iced::Event::Window(window::Event::Opened { size, .. }) => {
                    Some(Message::Resized(id, size.width, size.height))
                }
                iced::Event::Window(window::Event::Closed) => Some(Message::WindowClosed(id)),
                // The user clicked the window's X — wallstudio intercepts it
                // and parks in the tray instead of quitting (Steam-style).
                iced::Event::Window(window::Event::CloseRequested) => {
                    Some(Message::CloseRequested(id))
                }
                // Always track modifiers — even when a text field has focus —
                // so Ctrl/Shift multi-select works in the editor.
                iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                    Some(Message::ModifiersChanged(modifiers))
                }
                iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    // Still record modifiers on every key press.
                    if status == iced::event::Status::Captured {
                        // Don't run gallery shortcuts while typing, but keep mods.
                        return Some(Message::ModifiersChanged(modifiers));
                    }
                    Some(Message::Key(key, modifiers))
                }
                iced::Event::Keyboard(keyboard::Event::KeyReleased { modifiers, .. }) => {
                    Some(Message::ModifiersChanged(modifiers))
                }
                _ => None,
            }),
        ];
        // Right-click tray menu / click actions.
        subs.push(tray::subscription(self.tray_rx.clone()));
        subs.push(
            single_instance::subscription(self.single_instance_rx.clone()).map(|_| Message::Raise),
        );
        // Animation costs a full redraw per tick, so only run it when there is
        // something to animate — the editor's soft preview, or gallery GIFs.
        if self.settings.quality.preview_anim != PreviewAnim::Off
            || !self.editors.is_empty()
            || self.downloads.iter().any(|d| d.state.animates())
        {
            subs.push(
                iced::time::every(Duration::from_millis(self.settings.anim_interval_ms()))
                    .map(|_| Message::AnimTick),
            );
        }
        // Slider drags debounce their walld reload; this timer exists only
        // while an apply is actually queued.
        if self.pending_prop.is_some() {
            subs.push(
                iced::time::every(Duration::from_millis(PROP_FLUSH_TICK_MS))
                    .map(|_| Message::PropFlush),
            );
        }
        Subscription::batch(subs)
    }

    /// Close every window and keep wallstudio running from the tray, Steam-style.
    ///
    /// Wayland can't hide a window (winit's `set_visible` is a no-op there), so
    /// we destroy the surfaces and reopen them from the tray. All UI state
    /// lives in `App`, so reopening restores the same gallery/selection.
    fn hide_to_tray(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        if let Some(id) = self.library_id {
            self.library_id = None;
            tasks.push(window::close(id));
        }
        for (eid, _) in self.editors.drain() {
            tasks.push(window::close(eid));
        }
        self.last_msg = "wallstudio is in the tray — click its icon to reopen".into();
        self.last_ok = true;
        Task::batch(tasks)
    }

    /// Bring the library window back (reopening it if it was fully closed).
    fn show_from_tray(&mut self) -> Task<Message> {
        if let Some(id) = self.library_id {
            // A second launcher invocation is explicit user intent, so it is
            // appropriate to restore and focus the existing library window.
            Task::batch([
                window::set_mode(id, window::Mode::Windowed),
                window::gain_focus(id),
            ])
        } else {
            let (id, open) = window::open(window_settings(iced::Size::new(1280.0, 800.0)));
            self.library_id = Some(id);
            open.map(|_| Message::Tick)
        }
    }

    /// Push the current pause/mute state to the tray menu, only when it changed.
    fn sync_tray(&mut self) {
        let st = TrayState {
            paused: self.present.paused,
            muted: self.silent,
        };
        if self.tray_last == Some(st) {
            return;
        }
        self.tray_last = Some(st);
        if let Some(handle) = &self.tray {
            let _ = handle.update(|t| t.state = st);
        }
    }

    /// Handle a right-click tray menu action.
    fn update_tray(&mut self, ev: TrayEvent) -> Task<Message> {
        match ev {
            TrayEvent::Open => self.show_from_tray(),
            TrayEvent::TogglePause => {
                let v = !self.present.paused;
                self.present.paused = v;
                self.push_present("pause", if v { "1" } else { "0" });
                self.sync_tray();
                Task::none()
            }
            TrayEvent::ToggleMute => {
                let v = !self.silent;
                self.silent = v;
                self.present.mute = v;
                self.push_present("mute", if v { "1" } else { "0" });
                self.save_session();
                self.sync_tray();
                Task::none()
            }
            TrayEvent::Stop => {
                stop_all();
                self.runtime = status_snapshot();
                Task::none()
            }
            TrayEvent::Quit => {
                // Best-effort: drop the tray service, then leave. Wallpapers
                // keep playing via walld (same as closing used to behave).
                if let Some(handle) = &self.tray {
                    handle.shutdown().wait();
                }
                iced::exit()
            }
        }
    }

    fn open_editor_for_dir(&mut self, dir: std::path::PathBuf) -> Task<Message> {
        match open_project_dir(&dir) {
            Ok(scene) => {
                let session = EditorSession::open(scene);
                let (id, open) = window::open(window_settings(iced::Size::new(1440.0, 900.0)));
                self.editors.insert(id, session);
                self.last_msg = "opened scene editor".into();
                self.last_ok = true;
                open.map(move |_| Message::Tick)
            }
            Err(e) => {
                self.last_msg = e;
                self.last_ok = false;
                Task::none()
            }
        }
    }

    fn start_edit_selected(&mut self, fork_if_needed: bool) -> Task<Message> {
        let Some(e) = self.selected().cloned() else {
            self.last_msg = "nothing selected".into();
            self.last_ok = false;
            return Task::none();
        };
        if !is_editable_scene(&e.dir, e.project.wallpaper_type) {
            self.last_msg = "only Scene wallpapers can be edited".into();
            self.last_ok = false;
            return Task::none();
        }
        // Local forks / already-unpacked projects: open in place.
        let is_local = matches!(e.source, WeSource::LocalFolder | WeSource::MyProjects)
            && e.dir.join("scene.json").is_file()
            && !e.dir.join("scene.pkg").is_file();
        if is_local || !fork_if_needed {
            if e.dir.join("scene.json").is_file() && !e.has_scene_pkg {
                return self.open_editor_for_dir(e.dir.clone());
            }
        }
        // Workshop (or pkg-only): fork to wallengine projects.
        self.busy = true;
        let result = fork_wallpaper(&e.dir, &e.id);
        self.busy = false;
        match result {
            Ok(fork) => {
                self.last_msg = format!("forked → {}", fork.dir.display());
                self.last_ok = true;
                self.reload();
                self.open_editor_for_dir(fork.dir)
            }
            Err(err) => {
                self.last_msg = err;
                self.last_ok = false;
                Task::none()
            }
        }
    }

    pub fn visible(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        let mut idxs: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                if e.source == WeSource::Workshop
                    && self.downloads.iter().any(|d| d.item.id == e.id)
                {
                    return false;
                }
                if !self.filter_source_workshop && e.source == WeSource::Workshop {
                    return false;
                }
                if !self.filter_source_local
                    && matches!(e.source, WeSource::MyProjects | WeSource::LocalFolder)
                {
                    return false;
                }
                match self.filter_type {
                    TypeFilter::All => {}
                    TypeFilter::Scene => {
                        if e.project.wallpaper_type != WallpaperType::Scene && !e.has_scene_pkg {
                            return false;
                        }
                    }
                    TypeFilter::Video => {
                        if e.project.wallpaper_type != WallpaperType::Video {
                            return false;
                        }
                    }
                    TypeFilter::Web => {
                        if e.project.wallpaper_type != WallpaperType::Web {
                            return false;
                        }
                    }
                }
                // Genre tags from WE project.json `tags`. Empty selection = all.
                // Wallpaper must carry at least one selected tag (case-insensitive).
                if !self.filter_genres.is_empty() {
                    let tags = &e.project.tags;
                    let hit = self
                        .filter_genres
                        .iter()
                        .any(|g| tags.iter().any(|t| t.eq_ignore_ascii_case(g)));
                    // Untagged wallpapers only match when "Unspecified" is on.
                    let untagged = tags.is_empty();
                    let want_unspec = self
                        .filter_genres
                        .iter()
                        .any(|g| g.eq_ignore_ascii_case("Unspecified"));
                    if !hit && !(untagged && want_unspec) {
                        return false;
                    }
                }
                // Age rating from WE project.json `contentrating`. Empty = all.
                if !self.filter_ratings.is_empty() {
                    let rating = e.project.content_rating.as_deref().unwrap_or("Everyone");
                    let ok = self
                        .filter_ratings
                        .iter()
                        .any(|r| r.eq_ignore_ascii_case(rating));
                    if !ok {
                        return false;
                    }
                }
                if q.is_empty() {
                    return true;
                }
                e.project.title.to_lowercase().contains(&q)
                    || e.id.contains(&q)
                    || e.project.tags.iter().any(|t| t.to_lowercase().contains(&q))
                    || e.project
                        .content_rating
                        .as_ref()
                        .is_some_and(|r| r.to_lowercase().contains(&q))
            })
            .map(|(i, _)| i)
            .collect();
        if !self.sort_newest {
            idxs.sort_by(|&a, &b| {
                self.entries[a]
                    .project
                    .title
                    .to_lowercase()
                    .cmp(&self.entries[b].project.title.to_lowercase())
            });
        }
        idxs
    }

    /// How many library entries carry each stock genre tag (for sidebar counts).
    /// Counts come from each wallpaper's WE `project.json` tags.
    pub fn genre_counts(&self) -> HashMap<String, usize> {
        let mut m: HashMap<String, usize> = HashMap::new();
        for e in &self.entries {
            if e.project.tags.is_empty() {
                *m.entry("Unspecified".into()).or_default() += 1;
            }
            for t in &e.project.tags {
                // Normalize to official casing when possible.
                let key = WE_GENRE_TAGS
                    .iter()
                    .find(|g| g.eq_ignore_ascii_case(t))
                    .map(|s| (*s).to_string())
                    .unwrap_or_else(|| t.clone());
                *m.entry(key).or_default() += 1;
            }
        }
        m
    }

    /// Counts per WE age rating (`contentrating`).
    pub fn rating_counts(&self) -> HashMap<String, usize> {
        let mut m: HashMap<String, usize> = HashMap::new();
        for e in &self.entries {
            let raw = e.project.content_rating.as_deref().unwrap_or("Everyone");
            let key = WE_AGE_RATINGS
                .iter()
                .find(|r| r.eq_ignore_ascii_case(raw))
                .map(|s| (*s).to_string())
                .unwrap_or_else(|| raw.to_string());
            *m.entry(key).or_default() += 1;
        }
        m
    }

    pub fn selected(&self) -> Option<&WeEntry> {
        self.entries.get(self.cursor)
    }

    fn reload(&mut self) {
        self.entries = scan_all();
        self.entries.retain(|e| {
            e.source != WeSource::Workshop || !self.downloads.iter().any(|d| d.item.id == e.id)
        });
        self.monitors = discover_monitors_info();
        self.runtime = status_snapshot();
        self.workshop_pkg_count = workshop::count_local_workshop_packages();
        if self.cursor >= self.entries.len() && !self.entries.is_empty() {
            self.cursor = self.entries.len() - 1;
        }
        // Re-decode previews so regenerated preview.gif shows up.
        self.gifs.clear();
        self.snap_visible();
        self.reload_props();
        self.last_msg = format!(
            "{} wallpapers · workshop {}",
            self.entries.len(),
            we::workshop_dir().display()
        );
        self.last_ok = true;
    }

    fn snap_visible(&mut self) {
        let vis = self.visible();
        if vis.is_empty() {
            return;
        }
        if !vis.contains(&self.cursor) {
            self.cursor = vis[0];
        }
        // Selection may have jumped; keep settings for the visible tile.
        if self.selected().map(|e| e.id.as_str()) != Some(self.props_id.as_str()) {
            self.reload_props();
        }
    }

    /// Sidebar + detail + dividers + chrome subtracted from window width.
    pub fn gallery_width(&self) -> f32 {
        // Match ui.rs responsive side/detail widths.
        let side = if self.window_width < 900.0 {
            168.0
        } else {
            200.0
        };
        let detail = if self.window_width < 900.0 {
            260.0
        } else if self.window_width < 1100.0 {
            300.0
        } else {
            320.0
        };
        const DIVIDERS: f32 = 4.0;
        (self.window_width - side - detail - DIVIDERS).max(200.0)
    }

    /// (columns, cell_width including pad, thumb_height)
    pub fn grid_metrics(&self) -> (usize, f32, f32) {
        const PAD: f32 = 32.0; // gallery padding L+R
        const GAP: f32 = 16.0;
        // Tile size is a setting; the floor keeps captions readable.
        let target_cell = self.settings.quality.tile_size as f32;
        let min_cell = 160.0_f32.min(target_cell);
        let max_cell = (target_cell * 1.55).max(340.0);
        let avail = (self.gallery_width() - PAD).max(min_cell);
        let mut cols = ((avail + GAP) / (target_cell + GAP)).floor() as usize;
        cols = cols.clamp(1, 16);
        // Always fill the row — no dead space on the right when resized larger.
        let cell = (avail - GAP * (cols.saturating_sub(1) as f32)) / cols as f32;
        // If cells got huge (very wide window, few items), add columns down to ~MIN
        let mut cols = cols;
        let mut cell = cell;
        while cell > max_cell && cols < 16 {
            cols += 1;
            cell = (avail - GAP * (cols.saturating_sub(1) as f32)) / cols as f32;
        }
        while cell < min_cell && cols > 1 {
            cols -= 1;
            cell = (avail - GAP * (cols.saturating_sub(1) as f32)) / cols as f32;
        }
        let thumb_w = (cell - 8.0).max(100.0);
        let thumb_h = thumb_w * 0.625;
        (cols, cell, thumb_h)
    }

    fn cols(&self) -> usize {
        self.grid_metrics().0
    }

    fn apply_selected(&mut self) {
        self.apply_on_monitor(None);
    }

    fn apply_on_monitor(&mut self, monitor_override: Option<String>) {
        let Some(e) = self.entries.get(self.cursor).cloned() else {
            self.last_msg = "nothing selected".into();
            self.last_ok = false;
            return;
        };
        if e.source == WeSource::Workshop && self.downloads.iter().any(|d| d.item.id == e.id) {
            self.last_msg = "This wallpaper is still downloading".into();
            return;
        }
        self.play_entry_dir(
            e.dir.clone(),
            e.id.clone(),
            e.project.wallpaper_type,
            e.project.title.clone(),
            monitor_override,
        );
    }

    fn sync_all_present(&mut self) {
        let p = self.present.clone();
        let bin = we::walld_binary();
        let cmds = [
            ("pause", (p.paused as u8).to_string()),
            ("rate", format!("{:.4}", p.rate)),
            ("mute", (p.mute as u8).to_string()),
            ("fit", p.fit.as_str().to_string()),
            ("zoom", format!("{:.4}", p.zoom)),
            ("offset", format!("{:.4} {:.4}", p.pos_x, p.pos_y)),
            ("flip_h", (p.flip_h as u8).to_string()),
            ("flip_v", (p.flip_v as u8).to_string()),
        ];
        for (k, v) in cmds {
            let mut args = vec!["ctl".to_string(), "we_present".to_string()];
            if !self.monitor.is_empty() {
                args.push(self.monitor.clone());
            }
            args.push(k.to_string());
            args.push(v);
            let _ = std::process::Command::new(&bin).args(&args).output();
        }
    }

    /// Refresh layout controls from walld for the selected display (live state).
    fn pull_live_present(&mut self) {
        let bin = we::walld_binary();
        let out = if self.monitor.is_empty() {
            std::process::Command::new(&bin)
                .args(["ctl", "we_present"])
                .output()
        } else {
            std::process::Command::new(&bin)
                .args(["ctl", "we_present", &self.monitor])
                .output()
        };
        let Ok(o) = out else { return };
        if !o.status.success() {
            return;
        }
        let line = String::from_utf8_lossy(&o.stdout);
        if let Some(p) = parse_present_status_line(line.trim()) {
            self.present = p;
            self.silent = self.present.mute;
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => {
                if self.tab == MainTab::Workshop {
                    return self.workshop_fetch_task();
                }
                self.reload();
            }
            Message::Tick => {
                self.runtime = status_snapshot();
                // Keep the tray's Pause/Resume + Mute/Unmute labels honest.
                self.sync_tray();
                // Keep settings list aligned with selection (gallery clicks / filters).
                if self.selected().map(|e| e.id.as_str()) != Some(self.props_id.as_str()) {
                    self.reload_props();
                }
                // Following the *playing* wallpaper means the accent changes
                // when something else (a hotkey, walld itself) swaps it.
                self.refresh_palette(false);
                if !self.settings.behaviour.auto_refresh {
                    return self.poll_downloads();
                }
                // Steam may finish a workshop download while we're open — pick
                // it up without requiring a manual Refresh.
                let n = workshop::count_local_workshop_packages();
                if n != self.workshop_pkg_count {
                    let prev = self.workshop_pkg_count;
                    self.workshop_pkg_count = n;
                    self.reload();
                    if n > prev {
                        self.last_msg = format!(
                            "library · {} new package(s) from Steam · {} total",
                            n - prev,
                            n
                        );
                        self.last_ok = true;
                    }
                }
                return self.poll_downloads();
            }
            Message::AnimTick => {
                self.download_animation = (self.download_animation + 0.22) % std::f32::consts::TAU;
                for ed in self.editors.values_mut() {
                    let _ = ed.update(EditorMessage::AnimTick);
                }
            }
            Message::PropFlush => {
                // Debounced wallpaper prop hot-apply (~240ms after last change).
                if self.pending_prop.is_some() {
                    self.pending_prop_idle = self.pending_prop_idle.saturating_add(1);
                    if self.pending_prop_idle >= 2 {
                        self.flush_pending_prop();
                    }
                }
            }
            Message::Select(i) => {
                if i < self.entries.len() {
                    // Don't leave a prop apply for the previous wallpaper pending.
                    self.flush_pending_prop();
                    self.confirm = None;
                    self.cursor = i;
                    self.reload_props();
                }
            }
            Message::Move(dx, dy) => {
                let vis = self.visible();
                if vis.is_empty() {
                    return Task::none();
                }
                let cols = self.cols().max(1);
                let pos = vis.iter().position(|&i| i == self.cursor).unwrap_or(0);
                let row = pos / cols;
                let col = pos % cols;
                let rows = (vis.len() + cols - 1) / cols;
                let nr = (row as i32 + dy).clamp(0, rows.saturating_sub(1) as i32) as usize;
                let nc = (col as i32 + dx).clamp(0, (cols - 1) as i32) as usize;
                let mut np = nr * cols + nc;
                if np >= vis.len() {
                    np = vis.len() - 1;
                }
                self.confirm = None;
                self.cursor = vis[np];
                self.reload_props();
            }
            Message::Apply => self.apply_selected(),
            Message::ApplyOnMonitor(m) => self.apply_on_monitor(Some(m)),
            Message::Stop => {
                stop_all();
                self.runtime = status_snapshot();
                self.last_msg = "stopped".into();
                self.last_ok = true;
            }
            Message::SetMonitor(m) => {
                self.monitor = m;
                self.monitor_menu_open = false;
                // Show this display's present (flip/zoom may differ from the other).
                if let Some(e) = self.selected() {
                    self.present =
                        load_present_for(&e.id, &self.monitor, self.settings.behaviour.default_fit);
                    self.silent = self.present.mute;
                }
                self.pull_live_present();
                self.save_session();
            }
            Message::ToggleMonitorMenu => {
                self.monitor_menu_open = !self.monitor_menu_open;
                if self.monitor_menu_open {
                    self.play_monitor_menu_open = false;
                }
            }
            Message::TogglePlayMonitorMenu => {
                self.play_monitor_menu_open = !self.play_monitor_menu_open;
                if self.play_monitor_menu_open {
                    self.monitor_menu_open = false;
                }
            }
            Message::ToggleEngine => {
                self.engine = match self.engine {
                    PlayBackend::Lwe => PlayBackend::Walld,
                    _ => PlayBackend::Lwe,
                };
                self.save_session();
                self.last_msg = format!("engine · {}", self.engine.label());
                self.last_ok = true;
            }
            Message::SetTypeFilter(f) => {
                self.filter_type = f;
                self.snap_visible();
                self.save_session();
            }
            Message::ToggleWorkshop => {
                self.filter_source_workshop = !self.filter_source_workshop;
                self.snap_visible();
                self.save_session();
            }
            Message::ToggleLocal => {
                self.filter_source_local = !self.filter_source_local;
                self.snap_visible();
                self.save_session();
            }
            Message::ToggleGenre(tag) => {
                if self.filter_genres.contains(&tag) {
                    self.filter_genres.remove(&tag);
                } else {
                    self.filter_genres.insert(tag);
                }
                self.snap_visible();
                self.save_session();
            }
            Message::ClearGenres => {
                self.filter_genres.clear();
                self.snap_visible();
                self.save_session();
            }
            Message::ToggleRating(rating) => {
                if self.filter_ratings.contains(&rating) {
                    self.filter_ratings.remove(&rating);
                } else {
                    self.filter_ratings.insert(rating);
                }
                self.snap_visible();
                self.save_session();
            }
            Message::ClearRatings => {
                self.filter_ratings.clear();
                self.snap_visible();
                self.save_session();
            }
            Message::ToggleSilent => {
                self.silent = !self.silent;
                self.present.mute = self.silent;
                self.push_present("mute", if self.silent { "1" } else { "0" });
                self.save_session();
            }
            Message::ToggleSort => {
                self.sort_newest = !self.sort_newest;
                self.snap_visible();
                self.save_session();
            }
            Message::FilterChanged(s) => {
                self.filter = s;
                self.snap_visible();
                self.save_session();
            }
            Message::OpenFolder => {
                if let Some(e) = self.selected() {
                    let _ = std::process::Command::new("xdg-open").arg(&e.dir).spawn();
                } else {
                    let _ = std::process::Command::new("xdg-open")
                        .arg(we::workshop_dir())
                        .spawn();
                }
            }
            Message::Resized(id, w, h) => {
                // Editor windows must not resize the library gallery grid.
                if self.library_id == Some(id) {
                    // Allow very small windows (compact layout).
                    self.window_width = w.max(480.0);
                    self.window_height = h.max(360.0);
                }
            }
            Message::PropBool(key, v) => self.apply_prop(&key, json!(v), false),
            Message::PropSlider(key, v) => self.apply_prop(&key, json!(v), true),
            Message::PropText(key, v) => self.apply_prop(&key, json!(v), true),
            Message::PropCombo(key, v) => self.apply_prop(&key, json!(v), false),
            Message::PropColor(key, r, g, b) => {
                let s = format!("{r} {g} {b}");
                self.color_hex_draft = rgb_to_hex(r, g, b);
                self.apply_prop(&key, json!(s), true);
            }
            Message::PropColorHex(key, hex) => {
                self.color_hex_draft = hex.clone();
                if let Some([r, g, b]) = parse_hex_color(&hex) {
                    self.apply_prop(&key, json!(format!("{r} {g} {b}")), false);
                }
            }
            Message::ToggleColorOpen(key) => {
                if self.color_open.as_deref() == Some(key.as_str()) {
                    self.color_open = None;
                } else {
                    if let Some(PropValue::Color([r, g, b])) = self
                        .props
                        .iter()
                        .find(|p| p.key == key)
                        .map(|p| p.value.clone())
                    {
                        self.color_hex_draft = rgb_to_hex(r, g, b);
                    }
                    self.color_open = Some(key);
                }
            }
            Message::PropReset => {
                self.pending_prop = None;
                self.pending_prop_idle = 0;
                if let Some(e) = self.selected().cloned() {
                    match clear_overrides(&e.id) {
                        Ok(()) => {
                            self.reload_props();
                            let path = e.dir.display().to_string();
                            let bin = we::walld_binary();
                            let out = std::process::Command::new(&bin)
                                .args(["ctl", "we_reset_props", &path])
                                .output();
                            match out {
                                Ok(o) => {
                                    let msg = String::from_utf8_lossy(&o.stdout).trim().to_string();
                                    self.last_msg = if msg.is_empty() {
                                        "properties reset to defaults".into()
                                    } else {
                                        msg
                                    };
                                    self.last_ok = o.status.success();
                                }
                                Err(err) => {
                                    self.last_msg = format!("walld ctl: {err}");
                                    self.last_ok = false;
                                }
                            }
                            self.runtime = status_snapshot();
                        }
                        Err(err) => {
                            self.last_msg = err;
                            self.last_ok = false;
                        }
                    }
                }
            }
            Message::PresentPause(v) => {
                self.present.paused = v;
                self.push_present("pause", if v { "1" } else { "0" });
            }
            Message::PresentRate(v) => {
                self.present.rate = v.clamp(0.05, 4.0);
                self.push_present("rate", &format!("{:.4}", self.present.rate));
            }
            Message::PresentMute(v) => {
                self.present.mute = v;
                self.silent = v;
                self.push_present("mute", if v { "1" } else { "0" });
            }
            Message::PresentFit(f) => {
                self.present.fit = f;
                self.push_present("fit", f.as_str());
            }
            Message::PresentZoom(v) => {
                self.present.zoom = v.clamp(0.25, 4.0);
                self.push_present("zoom", &format!("{:.4}", self.present.zoom));
            }
            Message::PresentPosX(v) => {
                self.present.pos_x = v.clamp(-1.0, 1.0);
                self.push_present(
                    "offset",
                    &format!("{:.4} {:.4}", self.present.pos_x, self.present.pos_y),
                );
            }
            Message::PresentPosY(v) => {
                self.present.pos_y = v.clamp(-1.0, 1.0);
                self.push_present(
                    "offset",
                    &format!("{:.4} {:.4}", self.present.pos_x, self.present.pos_y),
                );
            }
            Message::PresentFlipH(v) => {
                self.present.flip_h = v;
                self.push_present("flip_h", if v { "1" } else { "0" });
            }
            Message::PresentFlipV(v) => {
                self.present.flip_v = v;
                self.push_present("flip_v", if v { "1" } else { "0" });
            }
            Message::PresentReset => {
                self.present = self.default_present();
                self.silent = true;
                let bin = we::walld_binary();
                let mut args = vec!["ctl".to_string(), "we_present".to_string()];
                if !self.monitor.is_empty() {
                    args.push(self.monitor.clone());
                }
                args.push("reset".into());
                let _ = std::process::Command::new(&bin).args(&args).output();
                // walld's reset returns to cover; re-assert the user's default.
                if self.present.fit != FitModeUi::Cover {
                    self.push_present("fit", self.present.fit.as_str());
                }
                self.last_msg = if self.monitor.is_empty() {
                    "presentation reset".into()
                } else {
                    format!("presentation reset · {}", self.monitor)
                };
                self.last_ok = true;
            }
            Message::Key(key, mods) => {
                self.keyboard_mods = mods;
                // Tool shortcuts when any editor is open (Roblox-style).
                if !self.editors.is_empty() {
                    use iced::keyboard::Key;
                    if let Key::Character(c) = &key {
                        let tool = match c.as_str() {
                            "v" | "V" => Some(Tool::Select),
                            "w" | "W" => Some(Tool::Move),
                            "e" | "E" if !mods.control() && !mods.command() => Some(Tool::Scale),
                            "r" | "R" if !mods.control() && !mods.command() => Some(Tool::Rotate),
                            _ => None,
                        };
                        if let Some(t) = tool {
                            for ed in self.editors.values_mut() {
                                ed.input_mods = mods;
                                let _ = ed.update(EditorMessage::SetTool(t));
                            }
                            return Task::none();
                        }
                        if (mods.control() || mods.command()) && c.eq_ignore_ascii_case("a") {
                            for ed in self.editors.values_mut() {
                                ed.input_mods = mods;
                                let _ = ed.update(EditorMessage::SelectAll);
                            }
                            return Task::none();
                        }
                        if (mods.control() || mods.command()) && c.eq_ignore_ascii_case("z") {
                            for ed in self.editors.values_mut() {
                                ed.input_mods = mods;
                                let _ = ed.update(if mods.shift() {
                                    EditorMessage::Redo
                                } else {
                                    EditorMessage::Undo
                                });
                            }
                            return Task::none();
                        }
                        if (mods.control() || mods.command()) && c.eq_ignore_ascii_case("s") {
                            for ed in self.editors.values_mut() {
                                ed.input_mods = mods;
                                let _ = ed.update(EditorMessage::Save);
                            }
                            return Task::none();
                        }
                    }
                }
                if self.library_id.is_none() {
                    return Task::none();
                }
                if mods.command() || mods.control() {
                    use iced::keyboard::Key;
                    if let Key::Character(c) = &key {
                        if c.as_str() == "," {
                            return Task::done(Message::SetTab(if self.tab == MainTab::Settings {
                                self.prev_tab
                            } else {
                                MainTab::Settings
                            }));
                        }
                        if c.eq_ignore_ascii_case("e") && self.tab == MainTab::Library {
                            return self.start_edit_selected(true);
                        }
                    }
                    return Task::none();
                }
                // The settings page has no gallery to drive; only leave it.
                if self.tab == MainTab::Settings {
                    use iced::keyboard::{key::Named, Key};
                    if matches!(key, Key::Named(Named::Escape)) {
                        return Task::done(Message::SetTab(self.prev_tab));
                    }
                    return Task::none();
                }
                // Escape backs out of an armed unsubscribe / delete.
                {
                    use iced::keyboard::{key::Named, Key};
                    if matches!(key, Key::Named(Named::Escape)) && self.confirm.is_some() {
                        return Task::done(Message::CancelConfirm);
                    }
                }
                // Gallery keys only when no editor tool consumed them.
                if !self.editors.is_empty() {
                    // Don't steal j/k etc. while editor open — still allow Enter play.
                    use iced::keyboard::{key::Named, Key};
                    if matches!(key, Key::Named(Named::Enter)) {
                        return Task::done(if self.tab == MainTab::Workshop {
                            Message::WorkshopSubscribe
                        } else {
                            Message::Apply
                        });
                    }
                    return Task::none();
                }
                use iced::keyboard::{key::Named, Key};
                if self.tab == MainTab::Workshop {
                    match key {
                        Key::Character(c) => match c.as_str() {
                            "j" | "J" => return Task::done(Message::WorkshopMove(0, 1)),
                            "k" | "K" => return Task::done(Message::WorkshopMove(0, -1)),
                            "h" | "H" => return Task::done(Message::WorkshopMove(-1, 0)),
                            "l" | "L" => return Task::done(Message::WorkshopMove(1, 0)),
                            "r" | "R" => return Task::done(Message::WorkshopRefresh),
                            "s" | "S" => return Task::done(Message::Stop),
                            "u" | "U" => return Task::done(Message::WorkshopUnsubscribe),
                            "[" => return Task::done(Message::WorkshopPage(-1)),
                            "]" => return Task::done(Message::WorkshopPage(1)),
                            _ => {}
                        },
                        Key::Named(Named::Enter) => {
                            // Installed → play; otherwise open Steam subscribe.
                            if self
                                .workshop
                                .selected()
                                .is_some_and(|i| workshop::is_subscribed(&i.id))
                            {
                                return Task::done(Message::WorkshopPlayInstalled);
                            }
                            return Task::done(Message::WorkshopSubscribe);
                        }
                        Key::Named(Named::ArrowDown) => {
                            return Task::done(Message::WorkshopMove(0, 1))
                        }
                        Key::Named(Named::ArrowUp) => {
                            return Task::done(Message::WorkshopMove(0, -1))
                        }
                        Key::Named(Named::ArrowLeft) => {
                            return Task::done(Message::WorkshopMove(-1, 0))
                        }
                        Key::Named(Named::ArrowRight) => {
                            return Task::done(Message::WorkshopMove(1, 0))
                        }
                        Key::Named(Named::PageUp) => return Task::done(Message::WorkshopPage(-1)),
                        Key::Named(Named::PageDown) => return Task::done(Message::WorkshopPage(1)),
                        _ => {}
                    }
                    return Task::none();
                }
                match key {
                    Key::Character(c) => match c.as_str() {
                        "j" | "J" => return Task::done(Message::Move(0, 1)),
                        "k" | "K" => return Task::done(Message::Move(0, -1)),
                        "h" | "H" => return Task::done(Message::Move(-1, 0)),
                        "l" | "L" => return Task::done(Message::Move(1, 0)),
                        "r" | "R" => return Task::done(Message::Refresh),
                        "u" | "U" => return Task::done(Message::LibraryUnsubscribe),
                        "o" | "O" => return Task::done(Message::OpenFolder),
                        "e" | "E" => return self.start_edit_selected(true),
                        _ => {}
                    },
                    Key::Named(Named::Enter) => return Task::done(Message::Apply),
                    Key::Named(Named::ArrowDown) => return Task::done(Message::Move(0, 1)),
                    Key::Named(Named::ArrowUp) => return Task::done(Message::Move(0, -1)),
                    Key::Named(Named::ArrowLeft) => return Task::done(Message::Move(-1, 0)),
                    Key::Named(Named::ArrowRight) => return Task::done(Message::Move(1, 0)),
                    _ => {}
                }
            }
            Message::EditScene => return self.start_edit_selected(true),
            Message::EditExisting => return self.start_edit_selected(false),
            Message::Editor(id, emsg) => {
                if matches!(emsg, EditorMessage::Close) {
                    self.editors.remove(&id);
                    // Pick up any new preview.gif in the library grid.
                    self.reload();
                    return window::close(id);
                }
                if let Some(ed) = self.editors.get_mut(&id) {
                    ed.input_mods = self.keyboard_mods;
                    if ed.update(emsg) {
                        self.editors.remove(&id);
                        self.reload();
                        return window::close(id);
                    }
                }
            }
            Message::WindowClosed(id) => {
                if self.library_id == Some(id) {
                    self.library_id = None;
                }
                self.editors.remove(&id);
                // Without a tray there's nothing to keep us alive, so the last
                // window closing still quits. With a tray we stay resident.
                if self.library_id.is_none() && self.editors.is_empty() && self.tray.is_none() {
                    return iced::exit();
                }
            }
            Message::CloseRequested(id) => {
                // Steam-style: closing the library window hides the whole app
                // into the tray. Editor windows still honour a real close.
                if self.library_id == Some(id) {
                    if self.tray.is_some() {
                        return self.hide_to_tray();
                    }
                    // No tray available — fall back to a real close + quit.
                    return window::close(id);
                }
                return window::close(id);
            }
            Message::Tray(ev) => {
                return self.update_tray(ev);
            }
            Message::Raise => return self.show_from_tray(),
            Message::ModifiersChanged(mods) => {
                self.keyboard_mods = mods;
                for ed in self.editors.values_mut() {
                    ed.input_mods = mods;
                }
            }
            Message::RenameDraft(s) => self.rename_draft = s,
            Message::RenameProject => {
                let Some(e) = self.selected().cloned() else {
                    self.last_msg = "nothing selected".into();
                    self.last_ok = false;
                    return Task::none();
                };
                if !is_local_project(&e.dir) {
                    self.last_msg =
                        "can only rename local wallstudio projects (not Steam workshop)".into();
                    self.last_ok = false;
                    return Task::none();
                }
                match rename_project_title(&e.dir, &self.rename_draft) {
                    Ok(()) => {
                        self.last_msg = format!("renamed → {}", self.rename_draft.trim());
                        self.last_ok = true;
                        self.reload();
                    }
                    Err(err) => {
                        self.last_msg = err;
                        self.last_ok = false;
                    }
                }
            }
            Message::DeleteProject => {
                let Some(e) = self.selected().cloned() else {
                    self.last_msg = "nothing selected".into();
                    self.last_ok = false;
                    return Task::none();
                };
                if !is_local_project(&e.dir) {
                    self.last_msg =
                        "can only delete local wallstudio projects (not Steam workshop)".into();
                    self.last_ok = false;
                    return Task::none();
                }
                if !self.arm(Confirm::DeleteProject(
                    e.dir.clone(),
                    e.project.title.clone(),
                )) {
                    return Task::none();
                }
                match delete_local_project(&e.dir) {
                    Ok(()) => {
                        self.last_msg = format!("deleted «{}»", e.project.title);
                        self.last_ok = true;
                        self.reload();
                    }
                    Err(err) => {
                        self.last_msg = err;
                        self.last_ok = false;
                    }
                }
            }
            Message::SetTab(tab) => {
                if self.tab == tab {
                    return Task::none();
                }
                self.confirm = None;
                if self.tab != MainTab::Settings {
                    self.prev_tab = self.tab;
                }
                self.tab = tab;
                // Workshop themes off the item you're browsing; the library
                // tab themes off the selected wallpaper. Re-derive either way.
                self.refresh_palette(false);
                match tab {
                    MainTab::Workshop => {
                        self.last_msg = "Steam Workshop · browse & subscribe".into();
                        self.last_ok = true;
                        if !self.workshop.loaded_once && !self.workshop.loading {
                            return self.workshop_fetch_task();
                        }
                    }
                    MainTab::Settings => {
                        self.accent_hex_draft =
                            theme::to_hex(self.settings.appearance.fixed_accent);
                        self.last_msg =
                            format!("settings · {}", settings::settings_path().display());
                        self.last_ok = true;
                    }
                    MainTab::Library => {
                        self.last_msg = "Installed library".into();
                        self.last_ok = true;
                    }
                }
            }
            Message::WorkshopSearch(s) => {
                self.workshop.search = s;
            }
            Message::WorkshopSetSort(s) => {
                self.workshop.sort = s;
                self.workshop.page = 1;
                return self.workshop_fetch_task();
            }
            Message::WorkshopSetType(t) => {
                self.workshop.filter_type = t;
                self.workshop.page = 1;
                return self.workshop_fetch_task();
            }
            Message::WorkshopToggleGenre(tag) => {
                if self.workshop.filter_genres.contains(&tag) {
                    self.workshop.filter_genres.remove(&tag);
                } else {
                    self.workshop.filter_genres.insert(tag);
                }
                self.workshop.page = 1;
                return self.workshop_fetch_task();
            }
            Message::WorkshopClearGenres => {
                self.workshop.filter_genres.clear();
                self.workshop.page = 1;
                return self.workshop_fetch_task();
            }
            Message::WorkshopToggleRating(rating) => {
                if self.workshop.filter_ratings.contains(&rating) {
                    self.workshop.filter_ratings.remove(&rating);
                } else {
                    self.workshop.filter_ratings.insert(rating);
                }
                self.workshop.page = 1;
                return self.workshop_fetch_task();
            }
            Message::WorkshopClearRatings => {
                self.workshop.filter_ratings.clear();
                self.workshop.page = 1;
                return self.workshop_fetch_task();
            }
            Message::WorkshopSelect(i) => {
                if i < self.workshop.items.len() {
                    self.confirm = None;
                    self.workshop.cursor = i;
                    self.refresh_palette(false);
                }
            }
            Message::WorkshopMove(dx, dy) => {
                if self.workshop.items.is_empty() {
                    return Task::none();
                }
                let cols = self.cols().max(1);
                let n = self.workshop.items.len();
                let pos = self.workshop.cursor.min(n - 1);
                let row = pos / cols;
                let col = pos % cols;
                let rows = (n + cols - 1) / cols;
                let nr = (row as i32 + dy).clamp(0, rows.saturating_sub(1) as i32) as usize;
                let nc = (col as i32 + dx).clamp(0, (cols - 1) as i32) as usize;
                let mut np = nr * cols + nc;
                if np >= n {
                    np = n - 1;
                }
                self.confirm = None;
                self.workshop.cursor = np;
                self.refresh_palette(false);
            }
            Message::WorkshopPage(delta) => {
                let next = (self.workshop.page as i32 + delta).max(1) as u32;
                if next == self.workshop.page {
                    return Task::none();
                }
                self.workshop.page = next;
                return self.workshop_fetch_task();
            }
            Message::WorkshopRefresh => {
                // Search submit / manual refresh: jump to page 1 and rescan local
                // library so "Installed" badges stay accurate after Steam downloads.
                self.workshop.page = 1;
                self.reload();
                return self.workshop_fetch_task();
            }
            Message::WorkshopLoaded(result) => {
                self.workshop.loading = false;
                match result {
                    Ok(page) => return self.apply_workshop_page(page),
                    Err(e) => {
                        self.workshop.error = Some(e.clone());
                        self.last_msg = e;
                        self.last_ok = false;
                    }
                }
            }
            Message::WorkshopPreviewReady(id, path) => {
                if let Some(item) = self.workshop.items.iter_mut().find(|i| i.id == id) {
                    if path.is_some() {
                        item.preview_path = path.clone();
                    }
                }
                if let Some(d) = self.downloads.iter_mut().find(|d| d.item.id == id) {
                    if path.is_some() {
                        d.item.preview_path = path;
                        downloads::save(&self.downloads);
                    }
                }
                // The themed item's preview may have only just landed.
                if self.workshop.selected().is_some_and(|i| i.id == id) {
                    self.refresh_palette(true);
                }
            }
            Message::WorkshopSubscribe => {
                let Some(item) = self.workshop.selected().cloned() else {
                    self.last_msg = "nothing selected".into();
                    self.last_ok = false;
                    return Task::none();
                };
                return self.subscribe_workshop_item(item);
            }
            Message::WorkshopSubscribed(id, result) => {
                if let Some(d) = self.downloads.iter_mut().find(|d| d.item.id == id) {
                    d.state = match result {
                        Ok(()) => downloads::State::Waiting,
                        Err(error) => downloads::State::Failed(error),
                    };
                    self.last_ok = !matches!(d.state, downloads::State::Failed(_));
                    self.last_msg = format!("{} · {}", d.item.title, d.state.label());
                    downloads::save(&self.downloads);
                }
                return self.poll_downloads();
            }
            Message::DownloadsPolled(snapshot) => {
                self.download_polling = false;
                let mut completed = Vec::new();
                let mut changed = false;
                for (id, state) in snapshot.states {
                    if let Some(d) = self.downloads.iter_mut().find(|d| d.item.id == id) {
                        if d.state == downloads::State::Subscribing {
                            continue;
                        }
                        if let Some(state) = state {
                            if matches!(d.state, downloads::State::Failed(_)) {
                                continue;
                            }
                            changed |= d.state != state;
                            d.state = state;
                        } else {
                            completed.push((id, d.item.title.clone()));
                        }
                    }
                }
                if !completed.is_empty() {
                    self.downloads
                        .retain(|d| !completed.iter().any(|(id, _)| id == &d.item.id));
                    self.reload();
                    self.last_msg = format!(
                        "Downloaded · {}",
                        completed
                            .iter()
                            .map(|(_, t)| t.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    self.last_ok = true;
                    changed = true;
                }
                if changed {
                    downloads::save(&self.downloads);
                }
            }
            Message::ShowDownload(id) => {
                if let Some(d) = self.downloads.iter().find(|d| d.item.id == id) {
                    let item = d.item.clone();
                    let idx = self
                        .workshop
                        .items
                        .iter()
                        .position(|i| i.id == id)
                        .unwrap_or_else(|| {
                            self.workshop.items.push(item);
                            self.workshop.items.len() - 1
                        });
                    self.workshop.cursor = idx;
                    self.tab = MainTab::Workshop;
                }
            }
            Message::WorkshopUnsubscribe => {
                let Some(item) = self.workshop.selected().cloned() else {
                    self.last_msg = "nothing selected".into();
                    self.last_ok = false;
                    return Task::none();
                };
                if !self.arm(Confirm::WorkshopUnsubscribe(
                    item.id.clone(),
                    item.title.clone(),
                )) {
                    return Task::none();
                }
                self.unsubscribe_workshop_id(&item.id, &item.title);
            }
            Message::LibraryUnsubscribe => {
                let Some(e) = self.selected().cloned() else {
                    self.last_msg = "nothing selected".into();
                    self.last_ok = false;
                    return Task::none();
                };
                if e.source != WeSource::Workshop {
                    self.last_msg =
                        "only Steam workshop items can be unsubscribed (local projects: Delete)"
                            .into();
                    self.last_ok = false;
                    return Task::none();
                }
                if !self.arm(Confirm::LibraryUnsubscribe(
                    e.id.clone(),
                    e.project.title.clone(),
                )) {
                    return Task::none();
                }
                self.unsubscribe_workshop_id(&e.id, &e.project.title);
            }
            Message::CancelConfirm => {
                if self.confirm.take().is_some() {
                    self.last_msg = "cancelled".into();
                    self.last_ok = true;
                }
            }
            Message::Settings(msg) => return self.update_settings(msg),
            Message::WorkshopOpenSteam => {
                let Some(item) = self.workshop.selected() else {
                    return Task::none();
                };
                match workshop::open_in_steam(&item.id) {
                    Ok(()) => {
                        self.last_msg = "opened in Steam client".into();
                        self.last_ok = true;
                    }
                    Err(e) => {
                        self.last_msg = e;
                        self.last_ok = false;
                    }
                }
            }
            Message::WorkshopOpenWeb => {
                let Some(item) = self.workshop.selected() else {
                    return Task::none();
                };
                match workshop::open_in_browser(&item.id) {
                    Ok(()) => {
                        self.last_msg = "opened workshop page in browser".into();
                        self.last_ok = true;
                    }
                    Err(e) => {
                        self.last_msg = e;
                        self.last_ok = false;
                    }
                }
            }
            Message::WorkshopPlayInstalled => {
                let Some(id) = self.workshop.selected().map(|i| i.id.clone()) else {
                    self.last_msg = "nothing selected".into();
                    self.last_ok = false;
                    return Task::none();
                };
                self.play_workshop_installed(&id);
            }
        }
        Task::none()
    }

    fn view(&self, id: window::Id) -> iced::Element<'_, Message> {
        if let Some(ed) = self.editors.get(&id) {
            return ed.view().map(move |m| Message::Editor(id, m));
        }
        ui::view(self)
    }
}

/// Session-wide wallstudio preferences (filters, engine, monitor).
#[derive(Debug, Clone)]
struct SessionPrefs {
    filter: String,
    filter_type: TypeFilter,
    filter_source_workshop: bool,
    filter_source_local: bool,
    filter_genres: BTreeSet<String>,
    filter_ratings: BTreeSet<String>,
    sort_newest: bool,
    monitor: String,
    silent: bool,
    engine: PlayBackend,
}

impl Default for SessionPrefs {
    fn default() -> Self {
        Self {
            filter: String::new(),
            filter_type: TypeFilter::All,
            filter_source_workshop: true,
            filter_source_local: true,
            filter_genres: BTreeSet::new(),
            filter_ratings: BTreeSet::new(),
            sort_newest: true,
            monitor: String::new(),
            silent: true,
            engine: PlayBackend::Walld,
        }
    }
}

fn wallstudio_config_dir() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            std::path::PathBuf::from(home).join(".config")
        });
    base.join("wallstudio")
}

fn session_prefs_path() -> std::path::PathBuf {
    wallstudio_config_dir().join("session.json")
}

fn load_session_prefs() -> SessionPrefs {
    let Ok(text) = std::fs::read_to_string(session_prefs_path()) else {
        return SessionPrefs::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return SessionPrefs::default();
    };
    let filter_type = match v
        .get("filter_type")
        .and_then(|x| x.as_str())
        .unwrap_or("all")
    {
        "scene" => TypeFilter::Scene,
        "video" => TypeFilter::Video,
        "web" => TypeFilter::Web,
        _ => TypeFilter::All,
    };
    let engine = match v.get("engine").and_then(|x| x.as_str()).unwrap_or("walld") {
        "lwe" | "LWE" | "linux-wallpaperengine" => PlayBackend::Lwe,
        _ => PlayBackend::Walld,
    };
    let mut genres = BTreeSet::new();
    if let Some(arr) = v.get("filter_genres").and_then(|x| x.as_array()) {
        for g in arr {
            if let Some(s) = g.as_str() {
                genres.insert(s.to_string());
            }
        }
    }
    let mut ratings = BTreeSet::new();
    if let Some(arr) = v.get("filter_ratings").and_then(|x| x.as_array()) {
        for g in arr {
            if let Some(s) = g.as_str() {
                ratings.insert(s.to_string());
            }
        }
    }
    SessionPrefs {
        filter: v
            .get("filter")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        filter_type,
        filter_source_workshop: v
            .get("filter_source_workshop")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        filter_source_local: v
            .get("filter_source_local")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        filter_genres: genres,
        filter_ratings: ratings,
        sort_newest: v
            .get("sort_newest")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        monitor: v
            .get("monitor")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        silent: v.get("silent").and_then(|x| x.as_bool()).unwrap_or(true),
        engine,
    }
}

fn save_session_prefs(p: &SessionPrefs) {
    let dir = wallstudio_config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let engine = match p.engine {
        PlayBackend::Lwe => "lwe",
        _ => "walld",
    };
    let filter_type = match p.filter_type {
        TypeFilter::Scene => "scene",
        TypeFilter::Video => "video",
        TypeFilter::Web => "web",
        TypeFilter::All => "all",
    };
    let v = serde_json::json!({
        "filter": p.filter,
        "filter_type": filter_type,
        "filter_source_workshop": p.filter_source_workshop,
        "filter_source_local": p.filter_source_local,
        "filter_genres": p.filter_genres.iter().cloned().collect::<Vec<_>>(),
        "filter_ratings": p.filter_ratings.iter().cloned().collect::<Vec<_>>(),
        "sort_newest": p.sort_newest,
        "monitor": p.monitor,
        "silent": p.silent,
        "engine": engine,
    });
    let _ = std::fs::write(
        session_prefs_path(),
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    );
}

/// walld's config path, for display in the Settings tab.
pub fn walld_config_display() -> String {
    walld_config_path().display().to_string()
}

fn walld_config_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            std::path::PathBuf::from(home).join(".config")
        });
    base.join("walld").join("config")
}

/// Set `key = value` lines in `~/.config/walld/config`, keeping every other
/// line (including comments and options wallstudio doesn't manage) intact.
fn write_walld_options(kv: &[(&str, String)]) -> Result<(), String> {
    let path = walld_config_path();
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let text = merge_walld_options(&existing, kv);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Rewrite `existing` with `kv` applied: managed keys are replaced in place,
/// everything else (comments, blank lines, other options) survives untouched.
fn merge_walld_options(existing: &str, kv: &[(&str, String)]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut written: BTreeSet<&str> = BTreeSet::new();
    for line in existing.lines() {
        let key = line
            .split('#')
            .next()
            .unwrap_or("")
            .split_once('=')
            .map(|(k, _)| k.trim());
        match key.and_then(|k| kv.iter().find(|(kk, _)| *kk == k)) {
            Some((k, v)) => {
                // Keep the first occurrence, drop later duplicates.
                if written.insert(k) {
                    out.push(format!("{k} = {v}"));
                }
            }
            None => out.push(line.to_string()),
        }
    }
    for (k, v) in kv {
        if !written.contains(k) {
            out.push(format!("{k} = {v}"));
        }
    }
    let mut text = out.join("\n");
    text.push('\n');
    text
}

fn present_path_for(id: &str) -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            std::path::PathBuf::from(home).join(".config")
        });
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    base.join("walld")
        .join("present")
        .join(format!("{safe}.json"))
}

/// Saved layout/playback for `id` on `monitor`. Wallpapers that were never
/// adjusted start from the user's configured default fit.
fn load_present_for(id: &str, monitor: &str, default_fit: FitModeUi) -> PresentSettings {
    let fallback = PresentSettings {
        fit: default_fit,
        ..PresentSettings::default()
    };
    let path = present_path_for(id);
    let Ok(text) = std::fs::read_to_string(path) else {
        return fallback;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return fallback;
    };
    // Prefer per-monitor visual block when a display is selected.
    let mon_v = if !monitor.is_empty() {
        v.get("monitors").and_then(|m| m.get(monitor)).cloned()
    } else {
        None
    };
    let visual = mon_v.as_ref().unwrap_or(&v);
    let fit = visual
        .get("fit")
        .or_else(|| v.get("fit"))
        .and_then(|x| x.as_str())
        .map(FitModeUi::parse)
        .unwrap_or(default_fit);
    PresentSettings {
        // Playback always from base.
        paused: v.get("paused").and_then(|x| x.as_bool()).unwrap_or(false),
        rate: v
            .get("rate")
            .and_then(|x| x.as_f64())
            .unwrap_or(1.0)
            .clamp(0.05, 4.0) as f32,
        mute: v.get("mute").and_then(|x| x.as_bool()).unwrap_or(true),
        fit,
        zoom: visual
            .get("zoom")
            .or_else(|| v.get("zoom"))
            .and_then(|x| x.as_f64())
            .unwrap_or(1.0)
            .clamp(0.25, 4.0) as f32,
        pos_x: visual
            .get("offset_x")
            .or_else(|| v.get("offset_x"))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0) as f32,
        pos_y: visual
            .get("offset_y")
            .or_else(|| v.get("offset_y"))
            .and_then(|x| x.as_f64())
            .unwrap_or(0.0)
            .clamp(-1.0, 1.0) as f32,
        flip_h: visual
            .get("flip_h")
            .or_else(|| v.get("flip_h"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
        flip_v: visual
            .get("flip_v")
            .or_else(|| v.get("flip_v"))
            .and_then(|x| x.as_bool())
            .unwrap_or(false),
    }
}

/// Parse `ok present paused=… flip_h=…` from `walld ctl we_present`.
fn parse_present_status_line(line: &str) -> Option<PresentSettings> {
    if !line.starts_with("ok present") {
        return None;
    }
    let mut map = std::collections::HashMap::new();
    for tok in line.split_whitespace().skip(2) {
        if let Some((k, v)) = tok.split_once('=') {
            map.insert(k, v);
        }
    }
    let fit = match map.get("fit").copied().unwrap_or("cover") {
        "contain" => FitModeUi::Contain,
        "fill" => FitModeUi::Fill,
        _ => FitModeUi::Cover,
    };
    let offset = map.get("offset").copied().unwrap_or("0,0");
    let (ox, oy) = {
        let parts: Vec<&str> = offset.split(|c| c == ',' || c == ' ').collect();
        (
            parts.first().and_then(|s| s.parse().ok()).unwrap_or(0.0),
            parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0.0),
        )
    };
    Some(PresentSettings {
        paused: map
            .get("paused")
            .map(|v| *v == "1" || *v == "true")
            .unwrap_or(false),
        rate: map.get("rate").and_then(|v| v.parse().ok()).unwrap_or(1.0),
        mute: map
            .get("mute")
            .map(|v| *v == "1" || *v == "true")
            .unwrap_or(true),
        fit,
        zoom: map.get("zoom").and_then(|v| v.parse().ok()).unwrap_or(1.0),
        pos_x: ox,
        pos_y: oy,
        flip_h: map
            .get("flip_h")
            .map(|v| *v == "1" || *v == "true")
            .unwrap_or(false),
        flip_v: map
            .get("flip_v")
            .map(|v| *v == "1" || *v == "true")
            .unwrap_or(false),
    })
}

fn rgb_to_hex(r: f32, g: f32, b: f32) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (r.clamp(0.0, 1.0) * 255.0).round() as u8,
        (g.clamp(0.0, 1.0) * 255.0).round() as u8,
        (b.clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

fn parse_hex_color(s: &str) -> Option<[f32; 3]> {
    let h = s.trim().trim_start_matches('#');
    if h.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()? as f32 / 255.0;
    let g = u8::from_str_radix(&h[2..4], 16).ok()? as f32 / 255.0;
    let b = u8::from_str_radix(&h[4..6], 16).ok()? as f32 / 255.0;
    Some([r, g, b])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_mode_round_trips() {
        for f in FitModeUi::ALL {
            assert_eq!(FitModeUi::parse(f.as_str()), f);
        }
        assert_eq!(FitModeUi::parse("nonsense"), FitModeUi::Cover);
    }

    #[test]
    fn walld_options_replace_in_place_and_keep_the_rest() {
        let existing = "\
# walld options
transition = wipe
scene_fps = 30
wipe_ms = 480
";
        let out = merge_walld_options(
            existing,
            &[
                ("scene_fps", "90".to_string()),
                ("video_max_edge", "2560".to_string()),
            ],
        );
        assert_eq!(
            out,
            "\
# walld options
transition = wipe
scene_fps = 90
wipe_ms = 480
video_max_edge = 2560
"
        );
    }

    #[test]
    fn walld_options_seed_an_empty_config() {
        let out = merge_walld_options("", &[("scene_fps", "60".to_string())]);
        assert_eq!(out, "scene_fps = 60\n");
    }

    #[test]
    fn walld_options_collapse_duplicate_keys() {
        let out = merge_walld_options(
            "scene_fps = 10\nscene_fps = 20\ntransition = snap\n",
            &[("scene_fps", "75".to_string())],
        );
        assert_eq!(out, "scene_fps = 75\ntransition = snap\n");
    }

    #[test]
    fn walld_options_survive_a_reread() {
        // Whatever we write must parse back to the same values walld will use.
        let text = merge_walld_options(
            "# comment\ntransition = snap\n",
            &[
                ("scene_fps", "45".to_string()),
                ("video_max_edge", "1920".to_string()),
            ],
        );
        let mut got = std::collections::HashMap::new();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            if let Some((k, v)) = line.split_once('=') {
                got.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
        assert_eq!(got.get("scene_fps").map(String::as_str), Some("45"));
        assert_eq!(got.get("video_max_edge").map(String::as_str), Some("1920"));
        assert_eq!(got.get("transition").map(String::as_str), Some("snap"));
    }

    #[test]
    fn confirm_prompts_name_the_target() {
        let c = Confirm::LibraryUnsubscribe("123".into(), "Neon City".into());
        assert!(c.prompt().contains("Neon City"));
        let d = Confirm::DeleteProject("/tmp/x".into(), "My Scene".into());
        assert!(d.prompt().contains("My Scene"));
        // Same action, different identity → not the same armed confirmation.
        assert_ne!(
            Confirm::LibraryUnsubscribe("1".into(), "a".into()),
            Confirm::LibraryUnsubscribe("2".into(), "a".into())
        );
    }
}
