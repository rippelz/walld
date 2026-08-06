//! wallstudio — iced control surface for the walld wallpaper engine.
//!
//! Gallery of scenes under `~/.local/share/wallengine/scenes`, apply via
//! `walld ctl scene`, live status from the daemon socket.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iced::widget::{button, column, container, row, scrollable, space, text, Column, Space};
use iced::{Alignment, Background, Border, Color, Element, Fill, Font, Length, Subscription, Task, Theme};

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window_size((920.0, 640.0))
        .run()
}

// ── model ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SceneEntry {
    name: String,
    path: PathBuf,
    /// short blurb: layer types / animated
    summary: String,
    animated: bool,
}

struct App {
    scenes: Vec<SceneEntry>,
    selected: Option<usize>,
    daemon_status: String,
    daemon_up: bool,
    last_action: String,
    busy: bool,
    scenes_dir: PathBuf,
}

#[derive(Debug, Clone)]
enum Message {
    RefreshScenes,
    RefreshStatus,
    Select(usize),
    ApplySelected,
    Apply(PathBuf),
    Tick,
    OpenScenesDir,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let scenes_dir = default_scenes_dir();
        let mut app = Self {
            scenes: Vec::new(),
            selected: None,
            daemon_status: String::new(),
            daemon_up: false,
            last_action: String::new(),
            busy: false,
            scenes_dir,
        };
        app.reload_scenes();
        app.reload_status();
        (app, Task::none())
    }

    fn title(&self) -> String {
        "wallstudio".into()
    }

    fn theme(&self) -> Theme {
        Theme::TokyoNight
    }

    fn subscription(&self) -> Subscription<Message> {
        iced::time::every(Duration::from_secs(2)).map(|_| Message::Tick)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::RefreshScenes => {
                self.reload_scenes();
                self.last_action = format!("scanned {} scene(s)", self.scenes.len());
            }
            Message::RefreshStatus | Message::Tick => {
                self.reload_status();
            }
            Message::Select(i) => {
                if i < self.scenes.len() {
                    self.selected = Some(i);
                }
            }
            Message::ApplySelected => {
                if let Some(i) = self.selected {
                    if let Some(s) = self.scenes.get(i).cloned() {
                        return Task::done(Message::Apply(s.path));
                    }
                }
            }
            Message::Apply(path) => {
                self.busy = true;
                let result = walld_ctl(&["scene", "*", &path.to_string_lossy()]);
                self.busy = false;
                match result {
                    Ok(out) => {
                        self.last_action = format!("apply: {}", out.trim());
                        self.reload_status();
                    }
                    Err(e) => {
                        self.last_action = format!("apply failed: {e}");
                        self.daemon_up = false;
                    }
                }
            }
            Message::OpenScenesDir => {
                let dir = self.scenes_dir.clone();
                let _ = Command::new("xdg-open").arg(&dir).spawn();
                self.last_action = format!("opened {}", dir.display());
            }
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let header = row![
            text("wallstudio").size(28).font(Font::DEFAULT),
            Space::new().width(12),
            text("wallpaper engine").size(14).color(MUTED),
            space::horizontal(),
            daemon_badge(self.daemon_up),
        ]
        .align_y(Alignment::Center)
        .spacing(8);

        let toolbar = row![
            button(text("Refresh scenes").size(14))
                .on_press(Message::RefreshScenes)
                .padding([8, 14])
                .style(secondary_btn),
            button(text("Refresh status").size(14))
                .on_press(Message::RefreshStatus)
                .padding([8, 14])
                .style(secondary_btn),
            button(text("Open folder").size(14))
                .on_press(Message::OpenScenesDir)
                .padding([8, 14])
                .style(secondary_btn),
            space::horizontal(),
            button(
                text(if self.busy { "Applying…" } else { "Apply to all monitors" }).size(14)
            )
            .on_press_maybe(if self.selected.is_some() && !self.busy {
                Some(Message::ApplySelected)
            } else {
                None
            })
            .padding([8, 16])
            .style(primary_btn),
        ]
        .spacing(8)
        .align_y(Alignment::Center);

        let list: Element<'_, Message> = if self.scenes.is_empty() {
            container(
                column![
                    text("No scenes found").size(18),
                    text(format!("Drop scene folders in:\n{}", self.scenes_dir.display()))
                        .size(13)
                        .color(MUTED),
                    button("Open folder").on_press(Message::OpenScenesDir).padding(10),
                ]
                .spacing(12)
                .padding(24),
            )
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .style(card_style)
            .into()
        } else {
            let items: Column<'_, Message> = self
                .scenes
                .iter()
                .enumerate()
                .fold(column![].spacing(8), |col, (i, s)| {
                    let selected = self.selected == Some(i);
                    col.push(scene_card(i, s, selected))
                });
            scrollable(container(items).padding(4).width(Fill))
                .height(Fill)
                .into()
        };

        let detail = detail_panel(self);

        let body = row![
            container(list).width(Length::FillPortion(3)).height(Fill),
            container(detail).width(Length::FillPortion(2)).height(Fill).padding(iced::Padding { top: 0.0, right: 0.0, bottom: 0.0, left: 12.0 }),
        ]
        .height(Fill);

        let status_bar = container(
            column![
                text(format!("daemon: {}", if self.daemon_status.is_empty() { "—" } else { &self.daemon_status }))
                    .size(12)
                    .font(Font::MONOSPACE),
                text(if self.last_action.is_empty() {
                    "ready"
                } else {
                    &self.last_action
                })
                .size(12)
                .color(MUTED),
            ]
            .spacing(4),
        )
        .width(Fill)
        .padding([10, 12])
        .style(status_style);

        let content = column![header, toolbar, body, status_bar]
            .spacing(14)
            .padding(18)
            .width(Fill)
            .height(Fill);

        container(content)
            .width(Fill)
            .height(Fill)
            .style(|_t| container::Style {
                background: Some(Background::Color(Color::from_rgb8(0x0d, 0x0f, 0x18))),
                ..Default::default()
            })
            .into()
    }

    fn reload_scenes(&mut self) {
        self.scenes = scan_scenes(&self.scenes_dir);
        if let Some(i) = self.selected {
            if i >= self.scenes.len() {
                self.selected = None;
            }
        }
    }

    fn reload_status(&mut self) {
        match walld_ctl(&["status"]) {
            Ok(out) => {
                self.daemon_up = out.trim_start().starts_with("ok");
                self.daemon_status = out.trim().to_string();
            }
            Err(e) => {
                self.daemon_up = false;
                self.daemon_status = format!("down ({e})");
            }
        }
    }
}

// ── widgets ────────────────────────────────────────────────────────────────

const MUTED: Color = Color::from_rgb(0.55, 0.58, 0.65);
const ACCENT: Color = Color::from_rgb(0.45, 0.72, 1.0);
const GOOD: Color = Color::from_rgb(0.45, 0.85, 0.55);
const BAD: Color = Color::from_rgb(0.95, 0.4, 0.4);

fn daemon_badge<'a>(up: bool) -> Element<'a, Message> {
    let (label, color) = if up {
        ("walld online", GOOD)
    } else {
        ("walld offline", BAD)
    };
    container(text(label).size(12).color(color))
        .padding([4, 10])
        .style(move |_t| container::Style {
            background: Some(Background::Color(Color::from_rgba(0.0, 0.0, 0.0, 0.35))),
            border: Border {
                color,
                width: 1.0,
                radius: 999.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn scene_card(i: usize, s: &SceneEntry, selected: bool) -> Element<'_, Message> {
    let badge = if s.animated {
        text("animated").size(11).color(ACCENT)
    } else {
        text("static").size(11).color(MUTED)
    };

    let body = column![
        row![text(&s.name).size(16), space::horizontal(), badge]
            .align_y(Alignment::Center),
        text(&s.summary).size(12).color(MUTED),
        text(s.path.display().to_string()).size(11).color(Color::from_rgb(0.35, 0.38, 0.45)),
    ]
    .spacing(4)
    .padding(14);

    button(body)
        .on_press(Message::Select(i))
        .width(Fill)
        .style(move |theme, status| {
            let mut st = if selected {
                button::Style {
                    background: Some(Background::Color(Color::from_rgb8(0x1a, 0x22, 0x38))),
                    border: Border {
                        color: ACCENT,
                        width: 1.5,
                        radius: 10.0.into(),
                    },
                    text_color: Color::WHITE,
                    ..button::primary(theme, status)
                }
            } else {
                button::Style {
                    background: Some(Background::Color(Color::from_rgb8(0x14, 0x17, 0x24))),
                    border: Border {
                        color: Color::from_rgb8(0x2a, 0x2f, 0x42),
                        width: 1.0,
                        radius: 10.0.into(),
                    },
                    text_color: Color::WHITE,
                    ..button::secondary(theme, status)
                }
            };
            if matches!(status, button::Status::Hovered) && !selected {
                st.border.color = Color::from_rgb8(0x4a, 0x55, 0x72);
            }
            st
        })
        .into()
}

fn detail_panel(app: &App) -> Element<'_, Message> {
    let inner: Element<'_, Message> = match app.selected.and_then(|i| app.scenes.get(i)) {
        Some(s) => column![
            text("Selected").size(12).color(MUTED),
            text(&s.name).size(22),
            Space::new().height(8),
            text(&s.summary).size(14),
            Space::new().height(12),
            text("path").size(12).color(MUTED),
            text(s.path.display().to_string())
                .size(12)
                .font(Font::MONOSPACE),
            Space::new().height(16),
            button(text("Apply scene").size(14))
                .on_press(Message::Apply(s.path.clone()))
                .padding([10, 16])
                .style(primary_btn),
            Space::new().height(8),
            text("Applies to all monitors via walld IPC.")
                .size(12)
                .color(MUTED),
        ]
        .spacing(4)
        .into(),
        None => column![
            text("No selection").size(18),
            text("Pick a scene from the list to preview details and apply.")
                .size(13)
                .color(MUTED),
        ]
        .spacing(8)
        .into(),
    };

    container(inner)
        .padding(16)
        .width(Fill)
        .height(Fill)
        .style(card_style)
        .into()
}

fn card_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb8(0x12, 0x15, 0x20))),
        border: Border {
            color: Color::from_rgb8(0x2a, 0x2f, 0x42),
            width: 1.0,
            radius: 12.0.into(),
        },
        ..Default::default()
    }
}

fn status_style(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Color::from_rgb8(0x10, 0x12, 0x1a))),
        border: Border {
            color: Color::from_rgb8(0x22, 0x26, 0x34),
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

fn primary_btn(theme: &Theme, status: button::Status) -> button::Style {
    let mut s = button::primary(theme, status);
    s.border.radius = 8.0.into();
    s
}

fn secondary_btn(theme: &Theme, status: button::Status) -> button::Style {
    let mut s = button::secondary(theme, status);
    s.border.radius = 8.0.into();
    s
}

// ── walld IPC + scene scan ─────────────────────────────────────────────────

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

fn scan_scenes(root: &Path) -> Vec<SceneEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        // Accept either dir/scene.json or a bare .json file
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

        let (name, summary, animated) = match wallengine_scene::SceneDocument::load_file(&scene_json) {
            Ok((doc, _)) => {
                let name = if doc.name.is_empty() {
                    path.file_stem()
                        .or_else(|| path.file_name())
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "unnamed".into())
                } else {
                    doc.name.clone()
                };
                let mut kinds = Vec::new();
                for l in &doc.layers {
                    let k = match l {
                        wallengine_scene::LayerDoc::Image { .. } => "image",
                        wallengine_scene::LayerDoc::Color { .. } => "color",
                        wallengine_scene::LayerDoc::Particles { .. } => "particles",
                    };
                    if !kinds.contains(&k) {
                        kinds.push(k);
                    }
                }
                let animated = doc.is_animated();
                let summary = format!(
                    "{} layer(s): {}",
                    doc.layers.len(),
                    if kinds.is_empty() {
                        "empty".into()
                    } else {
                        kinds.join(" · ")
                    }
                );
                (name, summary, animated)
            }
            Err(e) => {
                let name = scene_json
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "invalid".into());
                (name, format!("parse error: {e}"), false)
            }
        };

        out.push(SceneEntry {
            name,
            path: scene_json,
            summary,
            animated,
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}
