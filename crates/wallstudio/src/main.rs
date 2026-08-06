//! wallstudio — Wallpaper Engine browser & player for Hyprland.
//! The product is WE content. Native wallengine scenes are a secondary source.

mod ui;

use iced::{Subscription, Task, Theme};
use std::time::Duration;
use wallengine_we::{
    discover_monitors_info, play as we_play, scan_all, status_snapshot, stop_all, MonitorInfo,
    PlayRequest, RuntimeStatus, WallpaperType, WeEntry, WeSource,
};
use wallengine_we as we;

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window_size((1280.0, 800.0))
        .run()
}

pub struct App {
    pub entries: Vec<WeEntry>,
    pub cursor: usize,
    pub filter: String,
    pub filter_type: TypeFilter,
    pub filter_source_workshop: bool,
    pub filter_source_local: bool,
    pub sort_newest: bool,
    pub monitors: Vec<MonitorInfo>,
    /// empty = all
    pub monitor: String,
    pub silent: bool,
    pub runtime: RuntimeStatus,
    pub last_msg: String,
    pub last_ok: bool,
    pub busy: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeFilter {
    All,
    Scene,
    Video,
}

#[derive(Debug, Clone)]
pub enum Message {
    Refresh,
    Tick,
    FilterChanged(String),
    Select(usize),
    Move(i32, i32),
    Apply,
    Stop,
    SetMonitor(String),
    SetTypeFilter(TypeFilter),
    ToggleWorkshop,
    ToggleLocal,
    ToggleSilent,
    ToggleSort,
    OpenFolder,
    Key(iced::keyboard::Key, iced::keyboard::Modifiers),
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = Self {
            entries: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filter_type: TypeFilter::All,
            filter_source_workshop: true,
            filter_source_local: true,
            sort_newest: true,
            monitors: discover_monitors_info(),
            monitor: String::new(),
            silent: true,
            runtime: status_snapshot(),
            last_msg: "Wallpaper Engine library · select a tile · Enter to play".into(),
            last_ok: true,
            busy: false,
        };
        app.reload();
        (app, Task::none())
    }

    fn title(&self) -> String {
        "wallstudio — Wallpaper Engine".into()
    }

    fn theme(&self) -> Theme {
        Theme::Dark
    }

    fn subscription(&self) -> Subscription<Message> {
        use iced::event;
        use iced::keyboard;
        Subscription::batch([
            iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick),
            event::listen_with(|event, status, _id| {
                // Don't steal keys while a text input (or other widget) is focused.
                if status == iced::event::Status::Captured {
                    return None;
                }
                match event {
                    iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                        Some(Message::Key(key, modifiers))
                    }
                    _ => None,
                }
            }),
        ])
    }

    pub fn visible(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        let mut idxs: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
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
                        if e.project.wallpaper_type != WallpaperType::Scene
                            && !e.has_scene_pkg
                        {
                            return false;
                        }
                    }
                    TypeFilter::Video => {
                        if e.project.wallpaper_type != WallpaperType::Video {
                            return false;
                        }
                    }
                }
                if q.is_empty() {
                    return true;
                }
                e.project.title.to_lowercase().contains(&q)
                    || e.id.contains(&q)
                    || e.project.tags.iter().any(|t| t.to_lowercase().contains(&q))
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

    pub fn selected(&self) -> Option<&WeEntry> {
        self.entries.get(self.cursor)
    }

    fn reload(&mut self) {
        self.entries = scan_all();
        self.monitors = discover_monitors_info();
        self.runtime = status_snapshot();
        if self.cursor >= self.entries.len() && !self.entries.is_empty() {
            self.cursor = self.entries.len() - 1;
        }
        self.snap_visible();
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
    }

    fn cols(&self) -> usize {
        4
    }

    fn apply_selected(&mut self) {
        let Some(e) = self.entries.get(self.cursor).cloned() else {
            self.last_msg = "nothing selected".into();
            self.last_ok = false;
            return;
        };
        self.busy = true;
        let monitors = if self.monitor.is_empty() {
            Vec::new()
        } else {
            vec![self.monitor.clone()]
        };
        let req = PlayRequest {
            wallpaper_dir: e.dir.clone(),
            workshop_id: e.id.clone(),
            wallpaper_type: e.project.wallpaper_type,
            monitors,
            silent: self.silent,
            fps: 30,
        };
        match we_play(&req) {
            Ok(st) => {
                self.runtime = st;
                self.last_msg = format!(
                    "playing «{}» via {:?}",
                    e.project.title, self.runtime.backend
                );
                self.last_ok = true;
            }
            Err(err) => {
                self.last_msg = err.to_string();
                self.last_ok = false;
                self.runtime = status_snapshot();
            }
        }
        self.busy = false;
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Refresh => self.reload(),
            Message::Tick => self.runtime = status_snapshot(),
            Message::FilterChanged(s) => {
                self.filter = s;
                self.snap_visible();
            }
            Message::Select(i) => {
                if i < self.entries.len() {
                    self.cursor = i;
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
                self.cursor = vis[np];
            }
            Message::Apply => self.apply_selected(),
            Message::Stop => {
                stop_all();
                self.runtime = status_snapshot();
                self.last_msg = "stopped — walld surfaces restored".into();
                self.last_ok = true;
            }
            Message::SetMonitor(m) => self.monitor = m,
            Message::SetTypeFilter(f) => {
                self.filter_type = f;
                self.snap_visible();
            }
            Message::ToggleWorkshop => {
                self.filter_source_workshop = !self.filter_source_workshop;
                self.snap_visible();
            }
            Message::ToggleLocal => {
                self.filter_source_local = !self.filter_source_local;
                self.snap_visible();
            }
            Message::ToggleSilent => self.silent = !self.silent,
            Message::ToggleSort => {
                self.sort_newest = !self.sort_newest;
                self.snap_visible();
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
            Message::Key(key, mods) => {
                if mods.command() || mods.control() {
                    return Task::none();
                }
                use iced::keyboard::{key::Named, Key};
                match key {
                    Key::Character(c) => match c.as_str() {
                        "j" | "J" => return Task::done(Message::Move(0, 1)),
                        "k" | "K" => return Task::done(Message::Move(0, -1)),
                        "h" | "H" => return Task::done(Message::Move(-1, 0)),
                        "l" | "L" => return Task::done(Message::Move(1, 0)),
                        "r" | "R" => return Task::done(Message::Refresh),
                        "s" | "S" => return Task::done(Message::Stop),
                        "o" | "O" => return Task::done(Message::OpenFolder),
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
        }
        Task::none()
    }

    fn view(&self) -> iced::Element<'_, Message> {
        ui::view(self)
    }
}
