//! wallstudio — Wallpaper Engine–inspired browser for walld scenes.
//! Palette: ink panels + warm paper accent (no SaaS cyan).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iced::keyboard::{self, Key};
use iced::widget::image::Handle;
use iced::widget::{button, column, container, image, mouse_area, row, rule, scrollable, text, text_input, Space};
use iced::{
    Alignment, Background, Border, Color, Element, Event, Fill, Font, Length, Padding, Subscription,
    Task, Theme,
};
use iced::event;

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window_size((1180.0, 760.0))
        .run()
}

// ── palette ────────────────────────────────────────────────────────────────

mod pal {
    use iced::Color;
    pub const BG: Color = Color::from_rgb(0.07, 0.07, 0.08);
    pub const PANEL: Color = Color::from_rgb(0.10, 0.10, 0.11);
    pub const PANEL2: Color = Color::from_rgb(0.12, 0.12, 0.13);
    pub const TILE: Color = Color::from_rgb(0.09, 0.09, 0.10);
    pub const LINE: Color = Color::from_rgb(0.20, 0.20, 0.22);
    pub const LINE_HI: Color = Color::from_rgb(0.35, 0.35, 0.37);
    pub const FG: Color = Color::from_rgb(0.90, 0.90, 0.88);
    pub const DIM: Color = Color::from_rgb(0.52, 0.52, 0.50);
    pub const MUTE: Color = Color::from_rgb(0.38, 0.38, 0.36);
    pub const ACCENT: Color = Color::from_rgb(0.86, 0.72, 0.48);
    pub const OK: Color = Color::from_rgb(0.55, 0.72, 0.50);
    pub const ERR: Color = Color::from_rgb(0.82, 0.38, 0.35);
    pub const SELECT: Color = Color::from_rgb(0.86, 0.72, 0.48); // ring
    pub const SIDEBAR_W: f32 = 188.0;
    pub const DETAIL_W: f32 = 300.0;
    pub const THUMB: f32 = 168.0;
    pub const THUMB_H: f32 = 104.0;
}

// ── model ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortMode {
    Name,
    AnimatedFirst,
}

#[derive(Debug, Clone)]
struct LayerInfo {
    kind: String,
    detail: String,
}

#[derive(Debug, Clone)]
struct SceneEntry {
    name: String,
    path: PathBuf,
    preview: Option<PathBuf>,
    layers: Vec<LayerInfo>,
    animated: bool,
    layer_count: usize,
}

struct App {
    scenes: Vec<SceneEntry>,
    cursor: usize,
    filter: String,
    filter_animated: Option<bool>, // None = all, Some(true)=anim only, Some(false)=static
    filter_image: bool,
    filter_particles: bool,
    filter_color: bool,
    sort: SortMode,
    monitors: Vec<String>,
    /// empty string = all monitors (*)
    monitor: String,
    daemon_up: bool,
    active_scene: Option<String>,
    status_raw: String,
    last_msg: String,
    last_ok: bool,
    scenes_dir: PathBuf,
    focus_filter: bool,
}

#[derive(Debug, Clone)]
enum Message {
    FilterChanged(String),
    Select(usize),
    MoveGrid(i32, i32), // dx, dy
    Apply,
    Refresh,
    Tick,
    OpenDir,
    SetMonitor(String),
    ToggleAnimFilter,
    ToggleStaticFilter,
    ToggleImage,
    ToggleParticles,
    ToggleColor,
    CycleSort,
    Key(Key, keyboard::Modifiers),
    ClearFilters,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let mut app = Self {
            scenes: Vec::new(),
            cursor: 0,
            filter: String::new(),
            filter_animated: None,
            filter_image: false,
            filter_particles: false,
            filter_color: false,
            sort: SortMode::Name,
            monitors: discover_monitors(),
            monitor: String::new(), // all
            daemon_up: false,
            active_scene: None,
            status_raw: String::new(),
            last_msg: "click tile · double-click/enter apply · arrows navigate".into(),
            last_ok: true,
            scenes_dir: default_scenes_dir(),
            focus_filter: false,
        };
        app.reload_all();
        (app, Task::none())
    }

    fn title(&self) -> String {
        "wallstudio".into()
    }

    fn theme(&self) -> Theme {
        Theme::Dark
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick),
            event::listen_with(|event, _status, _id| match event {
                Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    Some(Message::Key(key, modifiers))
                }
                _ => None,
            }),
        ])
    }

    fn visible(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        let mut idxs: Vec<usize> = self
            .scenes
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                if !q.is_empty() && !s.name.to_lowercase().contains(&q) {
                    return false;
                }
                if let Some(anim) = self.filter_animated {
                    if s.animated != anim {
                        return false;
                    }
                }
                let type_filter =
                    self.filter_image || self.filter_particles || self.filter_color;
                if type_filter {
                    let kinds: Vec<&str> = s.layers.iter().map(|l| l.kind.as_str()).collect();
                    let ok = (self.filter_image && kinds.iter().any(|k| *k == "image"))
                        || (self.filter_particles && kinds.iter().any(|k| *k == "particles"))
                        || (self.filter_color && kinds.iter().any(|k| *k == "color"));
                    if !ok {
                        return false;
                    }
                }
                true
            })
            .map(|(i, _)| i)
            .collect();

        match self.sort {
            SortMode::Name => {
                idxs.sort_by(|&a, &b| {
                    self.scenes[a]
                        .name
                        .to_lowercase()
                        .cmp(&self.scenes[b].name.to_lowercase())
                });
            }
            SortMode::AnimatedFirst => {
                idxs.sort_by(|&a, &b| {
                    self.scenes[b]
                        .animated
                        .cmp(&self.scenes[a].animated)
                        .then_with(|| {
                            self.scenes[a]
                                .name
                                .to_lowercase()
                                .cmp(&self.scenes[b].name.to_lowercase())
                        })
                });
            }
        }
        idxs
    }

    fn cols(&self) -> usize {
        // approximate grid columns from typical window; fixed 4 is fine for prototype
        4
    }

    fn selected(&self) -> Option<&SceneEntry> {
        self.scenes.get(self.cursor)
    }

    fn reload_all(&mut self) {
        self.scenes = scan_scenes(&self.scenes_dir);
        self.monitors = discover_monitors();
        if self.cursor >= self.scenes.len() && !self.scenes.is_empty() {
            self.cursor = self.scenes.len() - 1;
        }
        if let Some(ref active) = self.active_scene {
            if let Some(i) = self.scenes.iter().position(|s| &s.name == active) {
                self.cursor = i;
            }
        }
        self.reload_status();
    }

    fn reload_status(&mut self) {
        match walld_ctl(&["status"]) {
            Ok(out) => {
                let line = out.trim().to_string();
                self.daemon_up = line.starts_with("ok");
                self.status_raw = line.clone();
                self.active_scene = parse_active_scene(&line);
            }
            Err(e) => {
                self.daemon_up = false;
                self.status_raw = e;
                self.active_scene = None;
            }
        }
    }

    fn apply_cursor(&mut self) {
        let Some(scene) = self.scenes.get(self.cursor).cloned() else {
            self.last_msg = "no scene selected".into();
            self.last_ok = false;
            return;
        };
        let mon = if self.monitor.is_empty() {
            "*"
        } else {
            self.monitor.as_str()
        };
        match walld_ctl(&["scene", mon, &scene.path.to_string_lossy()]) {
            Ok(out) => {
                self.last_msg = format!("applied «{}» → {mon}  {}", scene.name, out.trim());
                self.last_ok = true;
                self.reload_status();
            }
            Err(e) => {
                self.last_msg = format!("apply failed: {e}");
                self.last_ok = false;
            }
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::FilterChanged(s) => {
                self.filter = s;
                self.snap_cursor_visible();
            }
            Message::Select(i) => {
                if i < self.scenes.len() {
                    self.cursor = i;
                    self.focus_filter = false;
                }
            }
            Message::MoveGrid(dx, dy) => {
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
            Message::Apply => self.apply_cursor(),
            Message::Refresh => {
                self.reload_all();
                self.last_msg = format!("{} scene(s)", self.scenes.len());
                self.last_ok = true;
            }
            Message::Tick => self.reload_status(),
            Message::OpenDir => {
                let _ = Command::new("xdg-open").arg(&self.scenes_dir).spawn();
            }
            Message::SetMonitor(m) => self.monitor = m,
            Message::ToggleAnimFilter => {
                self.filter_animated = match self.filter_animated {
                    Some(true) => None,
                    _ => Some(true),
                };
                self.snap_cursor_visible();
            }
            Message::ToggleStaticFilter => {
                self.filter_animated = match self.filter_animated {
                    Some(false) => None,
                    _ => Some(false),
                };
                self.snap_cursor_visible();
            }
            Message::ToggleImage => {
                self.filter_image = !self.filter_image;
                self.snap_cursor_visible();
            }
            Message::ToggleParticles => {
                self.filter_particles = !self.filter_particles;
                self.snap_cursor_visible();
            }
            Message::ToggleColor => {
                self.filter_color = !self.filter_color;
                self.snap_cursor_visible();
            }
            Message::CycleSort => {
                self.sort = match self.sort {
                    SortMode::Name => SortMode::AnimatedFirst,
                    SortMode::AnimatedFirst => SortMode::Name,
                };
            }
            Message::ClearFilters => {
                self.filter.clear();
                self.filter_animated = None;
                self.filter_image = false;
                self.filter_particles = false;
                self.filter_color = false;
            }
            Message::Key(key, mods) => {
                if self.focus_filter {
                    if matches!(key, Key::Named(keyboard::key::Named::Escape)) {
                        self.focus_filter = false;
                        self.filter.clear();
                    }
                    return Task::none();
                }
                if mods.command() || mods.control() {
                    return Task::none();
                }
                match &key {
                    Key::Character(c) => match c.as_str() {
                        "j" | "J" => return Task::done(Message::MoveGrid(0, 1)),
                        "k" | "K" => return Task::done(Message::MoveGrid(0, -1)),
                        "h" | "H" => return Task::done(Message::MoveGrid(-1, 0)),
                        "l" | "L" => return Task::done(Message::MoveGrid(1, 0)),
                        "r" | "R" => return Task::done(Message::Refresh),
                        "o" | "O" => return Task::done(Message::OpenDir),
                        "/" => self.focus_filter = true,
                        _ => {}
                    },
                    Key::Named(keyboard::key::Named::Enter) => {
                        return Task::done(Message::Apply);
                    }
                    Key::Named(keyboard::key::Named::ArrowDown) => {
                        return Task::done(Message::MoveGrid(0, 1));
                    }
                    Key::Named(keyboard::key::Named::ArrowUp) => {
                        return Task::done(Message::MoveGrid(0, -1));
                    }
                    Key::Named(keyboard::key::Named::ArrowLeft) => {
                        return Task::done(Message::MoveGrid(-1, 0));
                    }
                    Key::Named(keyboard::key::Named::ArrowRight) => {
                        return Task::done(Message::MoveGrid(1, 0));
                    }
                    Key::Named(keyboard::key::Named::Escape) => self.filter.clear(),
                    _ => {}
                }
            }
        }
        Task::none()
    }

    fn snap_cursor_visible(&mut self) {
        let vis = self.visible();
        if vis.is_empty() {
            return;
        }
        if !vis.contains(&self.cursor) {
            self.cursor = vis[0];
        }
    }

    fn view(&self) -> Element<'_, Message> {
        container(
            column![
                top_bar(self),
                rule::horizontal(1).style(|_| rule_style()),
                row![
                    container(sidebar(self))
                        .width(Length::Fixed(pal::SIDEBAR_W))
                        .height(Fill)
                        .style(|_| panel(pal::PANEL)),
                    rule::vertical(1).style(|_| rule_style()),
                    container(gallery(self)).width(Fill).height(Fill).style(|_| panel(pal::BG)),
                    rule::vertical(1).style(|_| rule_style()),
                    container(detail(self))
                        .width(Length::Fixed(pal::DETAIL_W))
                        .height(Fill)
                        .style(|_| panel(pal::PANEL)),
                ]
                .height(Fill),
                rule::horizontal(1).style(|_| rule_style()),
                footer(self),
            ]
            .width(Fill)
            .height(Fill),
        )
        .width(Fill)
        .height(Fill)
        .style(|_| panel(pal::BG))
        .into()
    }
}

// ── chrome ─────────────────────────────────────────────────────────────────

fn top_bar(app: &App) -> Element<'_, Message> {
    let brand = row![
        text("WALL").size(16).color(pal::FG).font(Font::MONOSPACE),
        text("STUDIO").size(16).color(pal::ACCENT).font(Font::MONOSPACE),
    ];

    let mon_label = if app.monitor.is_empty() {
        "Monitor: All".to_string()
    } else {
        format!("Monitor: {}", app.monitor)
    };

    let mut mon_row = row![
        chip(
            "All",
            app.monitor.is_empty(),
            Message::SetMonitor(String::new()),
        ),
    ]
    .spacing(4);
    for m in &app.monitors {
        mon_row = mon_row.push(chip(
            m,
            app.monitor == *m,
            Message::SetMonitor(m.clone()),
        ));
    }

    let search = text_input("Search scenes…", &app.filter)
        .on_input(Message::FilterChanged)
        .on_submit(Message::Apply)
        .padding(8)
        .size(13)
        .width(Length::Fixed(220.0))
        .style(search_style);

    let sort_label = match app.sort {
        SortMode::Name => "Sort: Name",
        SortMode::AnimatedFirst => "Sort: Animated",
    };

    container(
        row![
            brand,
            Space::new().width(16),
            text(mon_label).size(12).color(pal::DIM).font(Font::MONOSPACE),
            Space::new().width(8),
            mon_row,
            Space::new().width(Fill),
            search,
            Space::new().width(8),
            flat_btn(sort_label, Message::CycleSort),
            Space::new().width(4),
            flat_btn("Refresh", Message::Refresh),
            Space::new().width(4),
            daemon_pill(app.daemon_up),
        ]
        .align_y(Alignment::Center)
        .spacing(0)
        .padding(Padding::from([10, 14])),
    )
    .width(Fill)
    .style(|_| panel(pal::PANEL))
    .into()
}

fn sidebar(app: &App) -> Element<'_, Message> {
    column![
        container(text("SHOW ONLY").size(10).color(pal::MUTE).font(Font::MONOSPACE))
            .padding(Padding { top: 12.0, right: 12.0, bottom: 6.0, left: 12.0 }),
        filter_row(
            "Animated",
            app.filter_animated == Some(true),
            Message::ToggleAnimFilter,
        ),
        filter_row(
            "Static",
            app.filter_animated == Some(false),
            Message::ToggleStaticFilter,
        ),
        container(text("LAYERS").size(10).color(pal::MUTE).font(Font::MONOSPACE))
            .padding(Padding { top: 14.0, right: 12.0, bottom: 6.0, left: 12.0 }),
        filter_row("Image", app.filter_image, Message::ToggleImage),
        filter_row("Particles", app.filter_particles, Message::ToggleParticles),
        filter_row("Color", app.filter_color, Message::ToggleColor),
        Space::new().height(12),
        container(flat_btn("Reset filters", Message::ClearFilters)).padding(Padding::from([0, 12])),
        Space::new().height(Fill),
        container(
            column![
                text("LIBRARY").size(10).color(pal::MUTE).font(Font::MONOSPACE),
                text(format!("{} scenes", app.scenes.len()))
                    .size(12)
                    .color(pal::DIM)
                    .font(Font::MONOSPACE),
                Space::new().height(6),
                flat_btn("Open folder", Message::OpenDir),
            ]
            .spacing(4),
        )
        .padding(12),
    ]
    .width(Fill)
    .height(Fill)
    .into()
}

fn gallery(app: &App) -> Element<'_, Message> {
    let vis = app.visible();
    let cols = app.cols();

    if vis.is_empty() {
        return container(
            column![
                text("No scenes match").size(16).color(pal::DIM),
                text(app.scenes_dir.display().to_string())
                    .size(12)
                    .color(pal::MUTE)
                    .font(Font::MONOSPACE),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .width(Fill)
        .height(Fill)
        .center_x(Fill)
        .center_y(Fill)
        .into();
    }

    let mut rows = column![].spacing(10).width(Fill);
    for chunk in vis.chunks(cols) {
        let mut r = row![].spacing(10);
        for &idx in chunk {
            r = r.push(thumb_tile(app, idx));
        }
        // pad incomplete row
        for _ in chunk.len()..cols {
            r = r.push(Space::new().width(Length::Fixed(pal::THUMB)));
        }
        rows = rows.push(r);
    }

    scrollable(
        container(rows)
            .padding(14)
            .width(Fill),
    )
    .height(Fill)
    .into()
}

fn thumb_tile(app: &App, idx: usize) -> Element<'_, Message> {
    let s = &app.scenes[idx];
    let selected = idx == app.cursor;
    let playing = app.active_scene.as_ref().is_some_and(|a| a == &s.name);

    let ring = if selected {
        pal::SELECT
    } else if playing {
        pal::OK
    } else {
        pal::LINE
    };
    let ring_w = if selected || playing { 2.0 } else { 1.0 };

    let preview: Element<'_, Message> = if let Some(ref p) = s.preview {
        container(
            image(Handle::from_path(p.clone()))
                .width(Length::Fixed(pal::THUMB - 4.0))
                .height(Length::Fixed(pal::THUMB_H - 4.0))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(pal::THUMB - 4.0))
        .height(Length::Fixed(pal::THUMB_H - 4.0))
        .center_x(Fill)
        .center_y(Fill)
        .into()
    } else {
        container(
            text(if s.animated { "ANIM" } else { "IMG" })
                .size(14)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
        )
        .width(Length::Fixed(pal::THUMB - 4.0))
        .height(Length::Fixed(pal::THUMB_H - 4.0))
        .center_x(Fill)
        .center_y(Fill)
        .style(|_| panel(pal::PANEL2))
        .into()
    };

    let media = container(
        column![
            // use a row overlay trick - just show preview full
            container(preview)
                .width(Length::Fixed(pal::THUMB - 4.0))
                .height(Length::Fixed(pal::THUMB_H - 4.0)),
        ],
    )
    .style(move |_| container::Style {
        background: Some(Background::Color(pal::TILE)),
        border: Border {
            color: ring,
            width: ring_w,
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    // restructure: media with badge overlay using stack isn't available - put badge above title
    let caption = column![
        row![
            text(&s.name)
                .size(12)
                .color(if selected { pal::FG } else { pal::DIM }),
            Space::new().width(Fill),
            if playing {
                text("●").size(10).color(pal::OK)
            } else if s.animated {
                text("A").size(10).color(pal::ACCENT).font(Font::MONOSPACE)
            } else {
                text(" ").size(10)
            },
        ]
        .align_y(Alignment::Center),
        text(format!("{} layers", s.layer_count))
            .size(10)
            .color(pal::MUTE)
            .font(Font::MONOSPACE),
    ]
    .spacing(2)
    .padding(Padding { top: 6.0, right: 2.0, bottom: 0.0, left: 2.0 });

    let body = column![media, caption].spacing(0).width(Length::Fixed(pal::THUMB));

    // click select, double-click apply via mouse_area
    mouse_area(body)
        .on_press(Message::Select(idx))
        .on_double_click(Message::Apply)
        .into()
}

fn detail(app: &App) -> Element<'_, Message> {
    let Some(s) = app.selected() else {
        return container(text("Select a wallpaper").size(13).color(pal::DIM))
            .padding(20)
            .into();
    };
    let playing = app.active_scene.as_ref().is_some_and(|a| a == &s.name);

    let preview: Element<'_, Message> = if let Some(ref p) = s.preview {
        container(
            image(Handle::from_path(p.clone()))
                .width(Fill)
                .height(Length::Fixed(160.0))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Fill)
        .height(Length::Fixed(160.0))
        .style(|_| container::Style {
            border: Border {
                color: pal::LINE,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
    } else {
        container(text("No preview").size(12).color(pal::MUTE).font(Font::MONOSPACE))
            .width(Fill)
            .height(Length::Fixed(160.0))
            .center_x(Fill)
            .center_y(Fill)
            .style(|_| panel(pal::PANEL2))
            .into()
    };

    let mut layers = column![
        text("LAYERS").size(10).color(pal::MUTE).font(Font::MONOSPACE),
        Space::new().height(4),
    ]
    .spacing(2)
    .width(Fill);
    for l in &s.layers {
        layers = layers.push(
            row![
                text(&l.kind)
                    .size(11)
                    .color(pal::ACCENT)
                    .font(Font::MONOSPACE)
                    .width(Length::Fixed(72.0)),
                text(&l.detail).size(11).color(pal::DIM).font(Font::MONOSPACE),
            ]
            .spacing(6),
        );
    }

    let mon = if app.monitor.is_empty() {
        "all monitors"
    } else {
        app.monitor.as_str()
    };

    let apply = button(
        container(
            text(if playing {
                format!("RELOAD · {mon}")
            } else {
                format!("APPLY · {mon}")
            })
            .size(13)
            .font(Font::MONOSPACE)
            .color(pal::BG),
        )
        .width(Fill)
        .center_x(Fill)
        .padding(12),
    )
    .on_press(Message::Apply)
    .padding(0)
    .width(Fill)
    .style(apply_style);

    column![
        preview,
        Space::new().height(14),
        text(&s.name).size(20).color(pal::FG),
        Space::new().height(4),
        text(if s.animated {
            "Animated scene"
        } else {
            "Static scene"
        })
        .size(12)
        .color(if s.animated { pal::ACCENT } else { pal::DIM })
        .font(Font::MONOSPACE),
        text(if playing { "ON DESKTOP" } else { "" })
            .size(11)
            .color(pal::OK)
            .font(Font::MONOSPACE),
        Space::new().height(12),
        layers,
        Space::new().height(12),
        text("PATH").size(10).color(pal::MUTE).font(Font::MONOSPACE),
        text(s.path.display().to_string())
            .size(10)
            .color(pal::MUTE)
            .font(Font::MONOSPACE),
        Space::new().height(Fill),
        apply,
        Space::new().height(8),
        text("Enter apply · double-click tile")
            .size(10)
            .color(pal::MUTE)
            .font(Font::MONOSPACE),
    ]
    .padding(14)
    .width(Fill)
    .height(Fill)
    .into()
}

fn footer(app: &App) -> Element<'_, Message> {
    let c = if app.last_ok { pal::DIM } else { pal::ERR };
    container(
        row![
            text(&app.last_msg)
                .size(11)
                .color(c)
                .font(Font::MONOSPACE)
                .width(Fill),
            text(truncate(&app.status_raw, 64))
                .size(11)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
        ]
        .padding(Padding::from([8, 14])),
    )
    .width(Fill)
    .style(|_| panel(pal::PANEL))
    .into()
}

// ── small widgets ──────────────────────────────────────────────────────────

fn filter_row(label: &str, on: bool, msg: Message) -> Element<'_, Message> {
    let mark = if on { "▣" } else { "□" };
    let color = if on { pal::ACCENT } else { pal::DIM };
    button(
        row![
            text(mark).size(13).color(color).font(Font::MONOSPACE),
            Space::new().width(8),
            text(label).size(13).color(if on { pal::FG } else { pal::DIM }),
        ]
        .align_y(Alignment::Center)
        .padding(Padding::from([6, 12])),
    )
    .on_press(msg)
    .padding(0)
    .width(Fill)
    .style(move |_t, status| {
        let bg = if matches!(status, button::Status::Hovered) {
            pal::PANEL2
        } else if on {
            Color::from_rgb(0.12, 0.11, 0.09)
        } else {
            Color::TRANSPARENT
        };
        button::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                width: 0.0,
                color: Color::TRANSPARENT,
                radius: 0.0.into(),
            },
            text_color: pal::FG,
            shadow: Default::default(),
            snap: true,
        }
    })
    .into()
}

fn chip(label: &str, on: bool, msg: Message) -> Element<'_, Message> {
    button(text(label).size(11).font(Font::MONOSPACE))
        .on_press(msg)
        .padding(Padding::from([4, 8]))
        .style(move |_t, status| {
            let (bg, border, fg) = if on {
                (Color::from_rgb(0.16, 0.14, 0.10), pal::ACCENT, pal::ACCENT)
            } else if matches!(status, button::Status::Hovered) {
                (pal::PANEL2, pal::LINE_HI, pal::FG)
            } else {
                (Color::TRANSPARENT, pal::LINE, pal::DIM)
            };
            button::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                text_color: fg,
                shadow: Default::default(),
                snap: true,
            }
        })
        .into()
}

fn flat_btn(label: impl Into<String>, msg: Message) -> Element<'static, Message> {
    let label = label.into();
    button(text(label).size(11).font(Font::MONOSPACE))
        .on_press(msg)
        .padding(Padding::from([6, 10]))
        .style(|_t, status| {
            let (bg, border) = match status {
                button::Status::Hovered => (pal::PANEL2, pal::LINE_HI),
                button::Status::Pressed => (pal::LINE, pal::ACCENT),
                _ => (Color::TRANSPARENT, pal::LINE),
            };
            button::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                text_color: pal::FG,
                shadow: Default::default(),
                snap: true,
            }
        })
        .into()
}

fn daemon_pill(up: bool) -> Element<'static, Message> {
    let (t, c) = if up {
        ("DAEMON OK", pal::OK)
    } else {
        ("DAEMON DOWN", pal::ERR)
    };
    container(text(t).size(10).color(c).font(Font::MONOSPACE))
        .padding(Padding::from([4, 8]))
        .style(move |_| container::Style {
            border: Border {
                color: c,
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn panel(bg: Color) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: 0.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn rule_style() -> rule::Style {
    rule::Style {
        color: pal::LINE,
        radius: 0.0.into(),
        fill_mode: rule::FillMode::Full,
        snap: true,
    }
}

fn search_style(_t: &Theme, status: text_input::Status) -> text_input::Style {
    let border = match status {
        text_input::Status::Focused { .. } => pal::ACCENT,
        _ => pal::LINE,
    };
    text_input::Style {
        background: Background::Color(pal::PANEL2),
        border: Border {
            color: border,
            width: 1.0,
            radius: 0.0.into(),
        },
        icon: pal::DIM,
        placeholder: pal::MUTE,
        value: pal::FG,
        selection: pal::ACCENT,
    }
}

fn apply_style(_t: &Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => Color::from_rgb(0.92, 0.80, 0.55),
        _ => pal::ACCENT,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: 0.0.into(),
            width: 0.0,
            color: bg,
        },
        text_color: pal::BG,
        shadow: Default::default(),
        snap: true,
    }
}

// ── domain ─────────────────────────────────────────────────────────────────

fn default_scenes_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("wallengine/scenes");
        }
    }
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into()))
        .join(".local/share/wallengine/scenes")
}

fn walld_bin() -> PathBuf {
    let local = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into()))
        .join(".local/bin/walld");
    if local.is_file() {
        local
    } else {
        PathBuf::from("walld")
    }
}

fn walld_ctl(args: &[&str]) -> Result<String, String> {
    let bin = walld_bin();
    let mut cmd = Command::new(&bin);
    cmd.arg("ctl");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().map_err(|e| format!("spawn {}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        Err(if !stdout.is_empty() {
            stdout
        } else if !stderr.is_empty() {
            stderr
        } else {
            format!("exit {}", out.status)
        }
        .trim()
        .to_string())
    }
}

fn parse_active_scene(status: &str) -> Option<String> {
    for part in status.split_whitespace() {
        if let Some(rest) = part.strip_prefix("scene=") {
            let name = rest.split('(').next().unwrap_or(rest).trim();
            if !name.is_empty() {
                return Some(name.to_string());
            }
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        format!("{}…", t.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn discover_monitors() -> Vec<String> {
    // Prefer hyprctl; fall back empty
    let out = Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok();
    let Some(out) = out else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name")?.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn scan_scenes(root: &Path) -> Vec<SceneEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        let scene_json = if path.is_dir() {
            let c = path.join("scene.json");
            if c.is_file() {
                c
            } else {
                continue;
            }
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            path.clone()
        } else {
            continue;
        };

        let base = scene_json.parent().unwrap_or(Path::new("."));
        // optional explicit preview
        let mut preview = ["preview.jpg", "preview.png", "thumb.jpg", "thumb.png"]
            .iter()
            .map(|n| base.join(n))
            .find(|p| p.is_file());

        let entry = match wallengine_scene::SceneDocument::load_file(&scene_json) {
            Ok((doc, base_dir)) => {
                let name = if doc.name.is_empty() {
                    base.file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "unnamed".into())
                } else {
                    doc.name.clone()
                };
                // first image layer as preview if no explicit
                if preview.is_none() {
                    for l in &doc.layers {
                        if let wallengine_scene::LayerDoc::Image { path, .. } = l {
                            let p = wallengine_scene::SceneDocument::resolve_path(&base_dir, path);
                            if p.is_file() {
                                preview = Some(p);
                                break;
                            }
                        }
                    }
                }
                let mut layers = Vec::new();
                for l in &doc.layers {
                    let (kind, detail) = match l {
                        wallengine_scene::LayerDoc::Image { path, fit, opacity, .. } => {
                            let leaf = Path::new(path)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone());
                            (
                                "image".into(),
                                format!("{leaf}  {fit:?}  {opacity:.0}%", opacity = opacity * 100.0),
                            )
                        }
                        wallengine_scene::LayerDoc::Color { color, opacity, .. } => (
                            "color".into(),
                            format!(
                                "rgb {:.0},{:.0},{:.0}  {opacity:.0}%",
                                color[0] * 255.0,
                                color[1] * 255.0,
                                color[2] * 255.0,
                                opacity = opacity * 100.0
                            ),
                        ),
                        wallengine_scene::LayerDoc::Particles {
                            preset, count, speed, ..
                        } => (
                            "particles".into(),
                            format!("{preset:?}  n={count}  spd={speed:.2}"),
                        ),
                    };
                    layers.push(LayerInfo { kind, detail });
                }
                SceneEntry {
                    name,
                    path: scene_json,
                    preview,
                    layer_count: layers.len(),
                    animated: doc.is_animated(),
                    layers,
                }
            }
            Err(e) => SceneEntry {
                name: base
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "error".into()),
                path: scene_json,
                preview: None,
                layers: vec![LayerInfo {
                    kind: "error".into(),
                    detail: e,
                }],
                animated: false,
                layer_count: 0,
            },
        };
        out.push(entry);
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}
