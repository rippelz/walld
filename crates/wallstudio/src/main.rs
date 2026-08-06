//! wallstudio — control surface for the walld wallpaper engine.
//!
//! Visual language: dense, square, high-contrast. Not a SaaS dashboard.
//! UX: keyboard-first list (j/k, Enter apply, r refresh), active scene
//! marked, structured inspector, no soft card soup.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iced::keyboard::{self, Key};
use iced::widget::{button, column, container, row, rule, scrollable, text, text_input, Space};
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
        .window_size((980.0, 680.0))
        .run()
}

// ── palette (ink terminal / rice-tool, not "product purple") ───────────────

mod pal {
    use iced::Color;
    pub const BG: Color = Color::from_rgb(0.06, 0.06, 0.07); // #0f0f12
    pub const PANEL: Color = Color::from_rgb(0.09, 0.09, 0.10);
    pub const PANEL2: Color = Color::from_rgb(0.11, 0.11, 0.12);
    pub const LINE: Color = Color::from_rgb(0.18, 0.18, 0.20);
    pub const LINE_HI: Color = Color::from_rgb(0.32, 0.32, 0.34);
    pub const FG: Color = Color::from_rgb(0.90, 0.90, 0.88);
    pub const DIM: Color = Color::from_rgb(0.48, 0.48, 0.46);
    pub const MUTE: Color = Color::from_rgb(0.35, 0.35, 0.34);
    /// Single accent — warm paper, not cyan SaaS
    pub const ACCENT: Color = Color::from_rgb(0.86, 0.72, 0.48); // #dbb87a
    pub const OK: Color = Color::from_rgb(0.55, 0.72, 0.50);
    pub const ERR: Color = Color::from_rgb(0.82, 0.38, 0.35);
    pub const SELECT: Color = Color::from_rgb(0.14, 0.13, 0.11);
}

// ── model ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct LayerInfo {
    kind: String,
    detail: String,
}

#[derive(Debug, Clone)]
struct SceneEntry {
    name: String,
    path: PathBuf,
    dir_label: String,
    layers: Vec<LayerInfo>,
    animated: bool,
    layer_count: usize,
}

struct App {
    scenes: Vec<SceneEntry>,
    /// Index into scenes, always valid when scenes non-empty.
    cursor: usize,
    filter: String,
    daemon_up: bool,
    /// Parsed bits from walld status
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
    Move(i32),
    Apply,
    Refresh,
    Tick,
    OpenDir,
    Key(Key, keyboard::Modifiers),
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let scenes_dir = default_scenes_dir();
        let mut app = Self {
            scenes: Vec::new(),
            cursor: 0,
            filter: String::new(),
            daemon_up: false,
            active_scene: None,
            status_raw: String::new(),
            last_msg: "j/k select · enter apply · / filter · r refresh".into(),
            last_ok: true,
            scenes_dir,
            focus_filter: false,
        };
        app.reload_all();
        (app, Task::none())
    }

    fn title(&self) -> String {
        "wallstudio".into()
    }

    fn theme(&self) -> Theme {
        // We paint everything ourselves; base theme is near-black.
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

    fn filtered_indices(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        self.scenes
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                q.is_empty()
                    || s.name.to_lowercase().contains(&q)
                    || s.dir_label.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn selected(&self) -> Option<&SceneEntry> {
        self.scenes.get(self.cursor)
    }

    fn reload_all(&mut self) {
        self.scenes = scan_scenes(&self.scenes_dir);
        if self.cursor >= self.scenes.len() && !self.scenes.is_empty() {
            self.cursor = self.scenes.len() - 1;
        }
        if self.scenes.is_empty() {
            self.cursor = 0;
        }
        // Prefer selecting currently active scene
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
        match walld_ctl(&["scene", "*", &scene.path.to_string_lossy()]) {
            Ok(out) => {
                self.last_msg = format!("applied «{}» — {}", scene.name, out.trim());
                self.last_ok = true;
                self.reload_status();
            }
            Err(e) => {
                self.last_msg = format!("apply failed: {e}");
                self.last_ok = false;
                self.daemon_up = false;
            }
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::FilterChanged(s) => {
                self.filter = s;
                // Keep cursor on first filtered hit if current not visible
                let vis = self.filtered_indices();
                if !vis.is_empty() && !vis.contains(&self.cursor) {
                    self.cursor = vis[0];
                }
            }
            Message::Select(i) => {
                if i < self.scenes.len() {
                    self.cursor = i;
                    self.focus_filter = false;
                }
            }
            Message::Move(delta) => {
                let vis = self.filtered_indices();
                if vis.is_empty() {
                    return Task::none();
                }
                let pos = vis.iter().position(|&i| i == self.cursor).unwrap_or(0);
                let n = vis.len() as i32;
                let next = (pos as i32 + delta).rem_euclid(n) as usize;
                self.cursor = vis[next];
            }
            Message::Apply => self.apply_cursor(),
            Message::Refresh => {
                self.reload_all();
                self.last_msg = format!("{} scene(s) · {}", self.scenes.len(), self.scenes_dir.display());
                self.last_ok = true;
            }
            Message::Tick => self.reload_status(),
            Message::OpenDir => {
                let _ = Command::new("xdg-open").arg(&self.scenes_dir).spawn();
                self.last_msg = format!("opened {}", self.scenes_dir.display());
                self.last_ok = true;
            }
            Message::Key(key, mods) => {
                if self.focus_filter {
                    // Let the text input handle typing; only Esc escapes filter.
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
                        "j" | "J" => return Task::done(Message::Move(1)),
                        "k" | "K" => return Task::done(Message::Move(-1)),
                        "r" | "R" => return Task::done(Message::Refresh),
                        "o" | "O" => return Task::done(Message::OpenDir),
                        "/" => {
                            self.focus_filter = true;
                        }
                        _ => {}
                    },
                    Key::Named(keyboard::key::Named::Enter) => {
                        return Task::done(Message::Apply);
                    }
                    Key::Named(keyboard::key::Named::ArrowDown) => {
                        return Task::done(Message::Move(1));
                    }
                    Key::Named(keyboard::key::Named::ArrowUp) => {
                        return Task::done(Message::Move(-1));
                    }
                    Key::Named(keyboard::key::Named::Escape) => {
                        self.filter.clear();
                    }
                    _ => {}
                }
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let top = top_bar(self);
        let body = row![
            container(scene_list(self))
                .width(Length::Fixed(340.0))
                .height(Fill)
                .style(|_| panel_style(pal::PANEL)),
            rule::vertical(1).style(|_| rule::Style { color: pal::LINE, radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true, }),
            container(inspector(self))
                .width(Fill)
                .height(Fill)
                .style(|_| panel_style(pal::BG)),
        ]
        .height(Fill);

        let footer = footer_bar(self);

        container(
            column![
                top,
                rule::horizontal(1).style(|_| rule_h()),
                body,
                rule::horizontal(1).style(|_| rule_h()),
                footer,
            ]
            .width(Fill)
            .height(Fill),
        )
        .width(Fill)
        .height(Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(pal::BG)),
            ..Default::default()
        })
        .into()
    }
}

// ── layout pieces ──────────────────────────────────────────────────────────

fn top_bar(app: &App) -> Element<'_, Message> {
    let brand = row![
        text("WALL").size(18).color(pal::FG).font(Font::MONOSPACE),
        text("STUDIO").size(18).color(pal::ACCENT).font(Font::MONOSPACE),
    ]
    .spacing(0);

    let daemon = if app.daemon_up {
        text("DAEMON  OK").size(12).color(pal::OK).font(Font::MONOSPACE)
    } else {
        text("DAEMON  DOWN").size(12).color(pal::ERR).font(Font::MONOSPACE)
    };

    let active = match &app.active_scene {
        Some(s) => text(format!("PLAYING  {s}"))
            .size(12)
            .color(pal::ACCENT)
            .font(Font::MONOSPACE),
        None => text("PLAYING  —")
            .size(12)
            .color(pal::DIM)
            .font(Font::MONOSPACE),
    };

    let filter = text_input("filter…", &app.filter)
        .on_input(Message::FilterChanged)
        .on_submit(Message::Apply)
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 10.0,
            right: 10.0,
        })
        .size(13)
        .width(Length::Fixed(200.0))
        .style(|_t, status| {
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
        });

    container(
        row![
            brand,
            Space::new().width(24),
            daemon,
            Space::new().width(16),
            active,
            Space::new().width(Fill),
            filter,
            Space::new().width(8),
            flat_btn("REFRESH", Message::Refresh),
            Space::new().width(4),
            flat_btn("FOLDER", Message::OpenDir),
        ]
        .align_y(Alignment::Center)
        .spacing(0)
        .padding(Padding {
            top: 10.0,
            bottom: 10.0,
            left: 14.0,
            right: 14.0,
        }),
    )
    .width(Fill)
    .style(|_| panel_style(pal::PANEL))
    .into()
}

fn scene_list(app: &App) -> Element<'_, Message> {
    let vis = app.filtered_indices();
    let header = container(
        row![
            text("SCENES")
                .size(11)
                .color(pal::DIM)
                .font(Font::MONOSPACE),
            Space::new().width(Fill),
            text(format!("{}/{}", vis.len(), app.scenes.len()))
                .size(11)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
        ]
        .padding(Padding {
            top: 10.0,
            bottom: 8.0,
            left: 12.0,
            right: 12.0,
        }),
    )
    .width(Fill)
    .style(|_| container::Style {
        border: Border {
            color: pal::LINE,
            width: 0.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    let mut list = column![].spacing(0).width(Fill);

    if vis.is_empty() {
        list = list.push(
            container(
                column![
                    text(if app.scenes.is_empty() {
                        "No scenes."
                    } else {
                        "No match."
                    })
                    .size(13)
                    .color(pal::DIM),
                    text(app.scenes_dir.display().to_string())
                        .size(11)
                        .color(pal::MUTE)
                        .font(Font::MONOSPACE),
                ]
                .spacing(6)
                .padding(16),
            )
            .width(Fill),
        );
    } else {
        for (row_i, &idx) in vis.iter().enumerate() {
            let s = &app.scenes[idx];
            let selected = idx == app.cursor;
            let playing = app.active_scene.as_ref().is_some_and(|a| a == &s.name);

            let marker = if selected { "▸" } else { " " };
            let flag = if s.animated { "A" } else { "S" };
            let play = if playing { "●" } else { " " };

            let line = row![
                text(marker).size(13).color(if selected { pal::ACCENT } else { pal::MUTE }).font(Font::MONOSPACE),
                Space::new().width(6),
                text(play).size(11).color(if playing { pal::OK } else { pal::MUTE }).font(Font::MONOSPACE),
                Space::new().width(8),
                column![
                    text(&s.name).size(14).color(if selected { pal::FG } else { Color::from_rgb(0.72, 0.72, 0.70) }),
                    text(format!(
                        "{flag}  {} layer{}",
                        s.layer_count,
                        if s.layer_count == 1 { "" } else { "s" }
                    ))
                    .size(11)
                    .color(pal::MUTE)
                    .font(Font::MONOSPACE),
                ]
                .spacing(2)
                .width(Fill),
            ]
            .align_y(Alignment::Center)
            .padding(Padding {
                top: 10.0,
                bottom: 10.0,
                left: 10.0,
                right: 10.0,
            });

            let bg = if selected {
                pal::SELECT
            } else if row_i % 2 == 1 {
                Color::from_rgb(0.08, 0.08, 0.09)
            } else {
                Color::TRANSPARENT
            };

            let left_border = if selected {
                pal::ACCENT
            } else if playing {
                pal::OK
            } else {
                Color::TRANSPARENT
            };

            list = list.push(
                button(line)
                    .on_press(Message::Select(idx))
                    .padding(0)
                    .width(Fill)
                    .style(move |_t, status| {
                        let mut background = bg;
                        if matches!(status, button::Status::Hovered) && !selected {
                            background = pal::PANEL2;
                        }
                        button::Style {
                            background: Some(Background::Color(background)),
                            border: Border {
                                color: left_border,
                                width: if left_border.a > 0.0 { 2.0 } else { 0.0 },
                                radius: 0.0.into(),
                            },
                            text_color: pal::FG,
                            shadow: Default::default(),
                            snap: true,
                        }
                    }),
            );
            list = list.push(
                rule::horizontal(1).style(|_| rule::Style { color: pal::LINE, radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true, }),
            );
        }
    }

    column![header, rule::horizontal(1).style(|_| rule_h()), scrollable(list).height(Fill)]
        .width(Fill)
        .height(Fill)
        .into()
}

fn inspector(app: &App) -> Element<'_, Message> {
    let Some(s) = app.selected() else {
        return container(
            text("Select a scene.")
                .size(14)
                .color(pal::DIM),
        )
        .padding(24)
        .into();
    };

    let playing = app.active_scene.as_ref().is_some_and(|a| a == &s.name);

    let title_row = row![
        column![
            text("SCENE").size(10).color(pal::MUTE).font(Font::MONOSPACE),
            text(&s.name).size(28).color(pal::FG),
        ]
        .spacing(4)
        .width(Fill),
        column![
            text(if s.animated { "ANIMATED" } else { "STATIC" })
                .size(11)
                .color(if s.animated { pal::ACCENT } else { pal::DIM })
                .font(Font::MONOSPACE),
            text(if playing { "ON DESKTOP" } else { "IDLE" })
                .size(11)
                .color(if playing { pal::OK } else { pal::MUTE })
                .font(Font::MONOSPACE),
        ]
        .spacing(4)
        .align_x(Alignment::End),
    ]
    .align_y(Alignment::End);

    // Layer table
    let mut layers = column![
        text("LAYERS").size(10).color(pal::MUTE).font(Font::MONOSPACE),
        Space::new().height(6),
        row![
            text("TYPE").size(10).color(pal::MUTE).font(Font::MONOSPACE).width(Length::Fixed(100.0)),
            text("DETAIL").size(10).color(pal::MUTE).font(Font::MONOSPACE).width(Fill),
        ],
        rule::horizontal(1).style(|_| rule_h()),
    ]
    .spacing(0)
    .width(Fill);

    for (i, layer) in s.layers.iter().enumerate() {
        let bg = if i % 2 == 0 {
            Color::TRANSPARENT
        } else {
            Color::from_rgb(0.08, 0.08, 0.085)
        };
        layers = layers.push(
            container(
                row![
                    text(&layer.kind)
                        .size(12)
                        .color(pal::ACCENT)
                        .font(Font::MONOSPACE)
                        .width(Length::Fixed(100.0)),
                    text(&layer.detail)
                        .size(12)
                        .color(pal::FG)
                        .font(Font::MONOSPACE)
                        .width(Fill),
                ]
                .padding(Padding {
                    top: 8.0,
                    bottom: 8.0,
                    left: 0.0,
                    right: 0.0,
                }),
            )
            .width(Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(bg)),
                ..Default::default()
            }),
        );
    }

    let path_block = column![
        text("PATH").size(10).color(pal::MUTE).font(Font::MONOSPACE),
        Space::new().height(4),
        text(s.path.display().to_string())
            .size(12)
            .color(pal::DIM)
            .font(Font::MONOSPACE),
    ]
    .spacing(0);

    let apply_label = if playing {
        "RELOAD SCENE  ↵"
    } else {
        "APPLY TO ALL MONITORS  ↵"
    };

    let apply = button(
        container(text(apply_label).size(13).font(Font::MONOSPACE).color(pal::BG))
            .width(Fill)
            .center_x(Fill)
            .padding(14),
    )
    .on_press(Message::Apply)
    .padding(0)
    .width(Fill)
    .style(|_t, status| {
        let bg = match status {
            button::Status::Hovered | button::Status::Pressed => Color::from_rgb(0.92, 0.80, 0.55),
            _ => pal::ACCENT,
        };
        button::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: bg,
                width: 0.0,
                radius: 0.0.into(),
            },
            text_color: pal::BG,
            shadow: Default::default(),
            snap: true,
        }
    });

    let keys = text("j/k  move    enter  apply    r  refresh    /  filter    o  folder")
        .size(11)
        .color(pal::MUTE)
        .font(Font::MONOSPACE);

    container(
        column![
            title_row,
            Space::new().height(20),
            layers,
            Space::new().height(20),
            path_block,
            Space::new().height(Fill),
            apply,
            Space::new().height(12),
            keys,
        ]
        .width(Fill)
        .height(Fill)
        .padding(Padding {
            top: 20.0,
            bottom: 16.0,
            left: 24.0,
            right: 24.0,
        }),
    )
    .width(Fill)
    .height(Fill)
    .into()
}

fn footer_bar(app: &App) -> Element<'_, Message> {
    let msg_color = if app.last_ok { pal::DIM } else { pal::ERR };
    container(
        row![
            text(if app.last_msg.is_empty() { "—" } else { &app.last_msg })
                .size(11)
                .color(msg_color)
                .font(Font::MONOSPACE)
                .width(Fill),
            text(truncate_status(&app.status_raw, 72))
                .size(11)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
        ]
        .align_y(Alignment::Center)
        .padding(Padding {
            top: 8.0,
            bottom: 8.0,
            left: 14.0,
            right: 14.0,
        }),
    )
    .width(Fill)
    .style(|_| panel_style(pal::PANEL))
    .into()
}

fn flat_btn(label: &'static str, msg: Message) -> Element<'static, Message> {
    button(text(label).size(11).font(Font::MONOSPACE))
        .on_press(msg)
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 10.0,
            right: 10.0,
        })
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

fn panel_style(bg: Color) -> container::Style {
    container::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: 0.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..Default::default()
    }
}

fn rule_h() -> rule::Style {
    rule::Style { color: pal::LINE, radius: 0.0.into(), fill_mode: rule::FillMode::Full, snap: true, }
}

// ── domain ─────────────────────────────────────────────────────────────────

fn default_scenes_dir() -> PathBuf {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return PathBuf::from(x).join("wallengine/scenes");
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    PathBuf::from(home).join(".local/share/wallengine/scenes")
}

fn walld_bin() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    let local = PathBuf::from(home).join(".local/bin/walld");
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
    let out = cmd
        .output()
        .map_err(|e| format!("spawn {}: {e}", bin.display()))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if out.status.success() {
        Ok(if stdout.is_empty() { stderr } else { stdout })
    } else {
        let msg = if !stdout.is_empty() {
            stdout
        } else if !stderr.is_empty() {
            stderr
        } else {
            format!("exit {}", out.status)
        };
        Err(msg.trim().to_string())
    }
}

fn parse_active_scene(status: &str) -> Option<String> {
    // ok scene=yozakura-snow (animated) DP-1=...
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

fn truncate_status(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        let head: String = t.chars().take(max.saturating_sub(1)).collect();
        format!("{head}…")
    }
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

        let dir_label = scene_json
            .parent()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();

        let entry = match wallengine_scene::SceneDocument::load_file(&scene_json) {
            Ok((doc, _)) => {
                let name = if doc.name.is_empty() {
                    dir_label.clone()
                } else {
                    doc.name.clone()
                };
                let mut layers = Vec::new();
                for l in &doc.layers {
                    let (kind, detail) = match l {
                        wallengine_scene::LayerDoc::Image { path, fit, opacity, id, .. } => {
                            let leaf = Path::new(path)
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| path.clone());
                            (
                                "image".into(),
                                format!(
                                    "{}  fit={:?}  opacity={opacity:.2}{}",
                                    leaf,
                                    fit,
                                    if id.is_empty() {
                                        String::new()
                                    } else {
                                        format!("  id={id}")
                                    }
                                ),
                            )
                        }
                        wallengine_scene::LayerDoc::Color { color, opacity, id, .. } => (
                            "color".into(),
                            format!(
                                "rgba({:.2},{:.2},{:.2},{:.2})  opacity={opacity:.2}{}",
                                color[0],
                                color[1],
                                color[2],
                                color[3],
                                if id.is_empty() {
                                    String::new()
                                } else {
                                    format!("  id={id}")
                                }
                            ),
                        ),
                        wallengine_scene::LayerDoc::Particles {
                            preset,
                            count,
                            speed,
                            opacity,
                            id,
                            ..
                        } => (
                            "particles".into(),
                            format!(
                                "{preset:?}  n={count}  speed={speed:.2}  opacity={opacity:.2}{}",
                                if id.is_empty() {
                                    String::new()
                                } else {
                                    format!("  id={id}")
                                }
                            ),
                        ),
                    };
                    layers.push(LayerInfo { kind, detail });
                }
                SceneEntry {
                    name,
                    path: scene_json,
                    dir_label,
                    layer_count: layers.len(),
                    animated: doc.is_animated(),
                    layers,
                }
            }
            Err(e) => SceneEntry {
                name: dir_label.clone(),
                path: scene_json,
                dir_label,
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
