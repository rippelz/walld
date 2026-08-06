//! Wallpaper Engine–style chrome for wallstudio.

use crate::{App, Message, TypeFilter};
use iced::widget::image::Handle;
use iced::widget::{
    button, column, container, image, mouse_area, row, rule, scrollable, text, text_input, Space,
};
use iced::{
    Alignment, Background, Border, Color, Element, Fill, Font, Length, Padding,
};
use wallengine_we::PlayBackend;
use wallengine_we::WallpaperType;
use wallengine_we::format_size;

mod pal {
    use iced::Color;
    pub const BG: Color = Color::from_rgb(0.07, 0.07, 0.08);
    pub const PANEL: Color = Color::from_rgb(0.10, 0.10, 0.11);
    pub const PANEL2: Color = Color::from_rgb(0.12, 0.12, 0.13);
    pub const LINE: Color = Color::from_rgb(0.20, 0.20, 0.22);
    pub const FG: Color = Color::from_rgb(0.90, 0.90, 0.88);
    pub const DIM: Color = Color::from_rgb(0.52, 0.52, 0.50);
    pub const MUTE: Color = Color::from_rgb(0.38, 0.38, 0.36);
    pub const ACCENT: Color = Color::from_rgb(0.86, 0.72, 0.48);
    pub const OK: Color = Color::from_rgb(0.55, 0.72, 0.50);
    pub const ERR: Color = Color::from_rgb(0.82, 0.38, 0.35);
    pub const SIDE: f32 = 220.0;
    pub const DETAIL: f32 = 320.0;
}

pub fn view(app: &App) -> Element<'_, Message> {
    container(
        column![
            top(app),
            hrule(),
            row![
                container(sidebar(app))
                    .width(Length::Fixed(pal::SIDE))
                    .height(Fill)
                    .style(|_| panel(pal::PANEL)),
                vrule(),
                container(gallery(app)).width(Fill).height(Fill).style(|_| panel(pal::BG)),
                vrule(),
                container(detail(app))
                    .width(Length::Fixed(pal::DETAIL))
                    .height(Fill)
                    .style(|_| panel(pal::PANEL)),
            ]
            .height(Fill),
            hrule(),
            footer(app),
        ]
        .width(Fill)
        .height(Fill),
    )
    .width(Fill)
    .height(Fill)
    .style(|_| panel(pal::BG))
    .into()
}

fn top(app: &App) -> Element<'_, Message> {
    let brand = row![
        text("WALL").size(16).color(pal::FG).font(Font::MONOSPACE),
        text("STUDIO").size(16).color(pal::ACCENT).font(Font::MONOSPACE),
    ]
    .align_y(Alignment::Center);

    // Library filter — installed/subscribed content, not remote Steam Workshop API
    let search = text_input("Filter installed…", &app.filter)
        .on_input(Message::FilterChanged)
        .on_submit(Message::Apply)
        .padding(8)
        .size(13)
        .width(Length::Fixed(260.0))
        .style(search_style);

    let be = text(if app.runtime.engine_ready {
        "ENGINE  walld"
    } else {
        "ENGINE  offline"
    })
    .size(11)
    .color(if app.runtime.engine_ready {
        pal::OK
    } else {
        pal::ERR
    })
    .font(Font::MONOSPACE);

    container(
        row![
            brand,
            Space::new().width(16),
            text("Installed library")
                .size(11)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
            Space::new().width(Fill),
            search,
            Space::new().width(8),
            flat("Refresh", Message::Refresh),
            Space::new().width(4),
            flat("Stop", Message::Stop),
            Space::new().width(12),
            be,
        ]
        .align_y(Alignment::Center)
        .padding(Padding::from([10, 14])),
    )
    .width(Fill)
    .style(|_| panel(pal::PANEL))
    .into()
}

fn sidebar(app: &App) -> Element<'_, Message> {
    column![
        label("DISPLAY"),
        display_picker(app),
        Space::new().height(8),
        label("TYPE"),
        filter_btn("All", app.filter_type == TypeFilter::All, Message::SetTypeFilter(TypeFilter::All)),
        filter_btn("Scene (.pkg)", app.filter_type == TypeFilter::Scene, Message::SetTypeFilter(TypeFilter::Scene)),
        filter_btn("Video", app.filter_type == TypeFilter::Video, Message::SetTypeFilter(TypeFilter::Video)),
        Space::new().height(12),
        label("SOURCE"),
        filter_btn("Steam (subscribed)", app.filter_source_workshop, Message::ToggleWorkshop),
        filter_btn("My projects", app.filter_source_local, Message::ToggleLocal),
        Space::new().height(12),
        label("OPTIONS"),
        filter_btn(
            if app.silent { "Audio muted" } else { "Audio on" },
            !app.silent,
            Message::ToggleSilent,
        ),
        filter_btn(
            if app.sort_newest { "Sort: newest id" } else { "Sort: name" },
            true,
            Message::ToggleSort,
        ),
        Space::new().height(Fill),
        container(
            column![
                label("LIBRARY"),
                text(format!("{} items", app.entries.len()))
                    .size(12)
                    .color(pal::DIM)
                    .font(Font::MONOSPACE),
                text(format!("{} shown", app.visible().len()))
                    .size(11)
                    .color(pal::MUTE)
                    .font(Font::MONOSPACE),
                Space::new().height(8),
                flat("Open folder", Message::OpenFolder),
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
    let (cols, cell_w, thumb_h) = app.grid_metrics();
    let thumb_w = (cell_w - 8.0).max(120.0);
    if vis.is_empty() {
        return container(
            column![
                text("No wallpapers found").size(18).color(pal::DIM),
                text("Subscribe to wallpapers in Steam, then Refresh.")
                    .size(13)
                    .color(pal::MUTE),
                text(wallengine_we::workshop_dir().display().to_string())
                    .size(11)
                    .color(pal::MUTE)
                    .font(Font::MONOSPACE),
                Space::new().height(12),
                text("Subscribe in Steam · walld renders content in-process")
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

    let gap = 16.0_f32;
    let mut rows = column![].spacing(gap).width(Fill);
    for chunk in vis.chunks(cols) {
        let mut r = row![].spacing(gap);
        for &idx in chunk {
            r = r.push(tile(app, idx, thumb_w, thumb_h, cell_w));
        }
        for _ in chunk.len()..cols {
            r = r.push(
                Space::new()
                    .width(Length::Fixed(cell_w))
                    .height(Length::Fixed(thumb_h + 48.0)),
            );
        }
        rows = rows.push(r);
    }
    scrollable(
        container(rows)
            .padding(Padding {
                top: 16.0,
                right: 16.0,
                bottom: 24.0,
                left: 16.0,
            })
            .width(Fill),
    )
    .height(Fill)
    .into()
}

fn tile(app: &App, idx: usize, thumb_w: f32, thumb_h: f32, cell_w: f32) -> Element<'_, Message> {
    let e = &app.entries[idx];
    let selected = idx == app.cursor;
    let playing = app.runtime.playing
        && (app.runtime.title == e.id || app.runtime.detail.contains(&e.id));

    let img_w = (thumb_w - 2.0).max(80.0);
    let img_h = (thumb_h - 2.0).max(50.0);

    let preview: Element<'_, Message> = if let Some(ref p) = e.preview {
        container(
            image(Handle::from_path(p.clone()))
                .width(Length::Fixed(img_w))
                .height(Length::Fixed(img_h))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(img_w))
        .height(Length::Fixed(img_h))
        .into()
    } else {
        container(
            text(e.project.wallpaper_type.as_label())
                .size(14)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
        )
        .width(Length::Fixed(img_w))
        .height(Length::Fixed(img_h))
        .center_x(Fill)
        .center_y(Fill)
        .style(|_| panel(pal::PANEL2))
        .into()
    };

    let media = container(preview).style(|_| container::Style {
        background: Some(Background::Color(pal::PANEL2)),
        border: Border {
            color: pal::LINE,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    let ty = e.project.wallpaper_type.as_label();
    let max_chars = ((thumb_w / 6.5) as usize).clamp(12, 48);
    let title = truncate_chars(&e.project.title, max_chars);
    let caption = column![
        text(title)
            .size(12)
            .color(if selected { pal::FG } else { pal::DIM }),
        row![
            text(ty).size(10).color(pal::ACCENT).font(Font::MONOSPACE),
            Space::new().width(Fill),
            text(format_size(e.size_bytes))
                .size(10)
                .color(pal::MUTE)
                .font(Font::MONOSPACE),
            if playing {
                text("  ●").size(10).color(pal::OK)
            } else {
                text("")
            },
        ],
    ]
    .spacing(3)
    .width(Length::Fixed(thumb_w))
    .padding(Padding {
        top: 8.0,
        right: 4.0,
        bottom: 2.0,
        left: 4.0,
    });

    let cell = container(column![media, caption].spacing(0).width(Length::Fixed(thumb_w)))
        .width(Length::Fixed(cell_w))
        .padding(4)
        .style(move |_| container::Style {
            background: Some(Background::Color(if selected {
                Color::from_rgb(0.12, 0.11, 0.09)
            } else {
                Color::TRANSPARENT
            })),
            border: Border {
                color: if selected {
                    pal::ACCENT
                } else if playing {
                    pal::OK
                } else {
                    Color::TRANSPARENT
                },
                width: if selected || playing { 1.5 } else { 0.0 },
                radius: 0.0.into(),
            },
            ..Default::default()
        });

    mouse_area(cell)
        .on_press(Message::Select(idx))
        .on_double_click(Message::Apply)
        .into()
}

fn detail(app: &App) -> Element<'_, Message> {
    let Some(e) = app.selected() else {
        return container(text("Select a wallpaper").size(14).color(pal::DIM))
            .padding(20)
            .into();
    };

    let preview: Element<'_, Message> = if let Some(ref p) = e.preview {
        container(
            image(Handle::from_path(p.clone()))
                .width(Fill)
                .height(Length::Fixed(180.0))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Fill)
        .height(Length::Fixed(180.0))
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
        container(text("No preview").size(12).color(pal::MUTE))
            .width(Fill)
            .height(Length::Fixed(180.0))
            .center_x(Fill)
            .center_y(Fill)
            .style(|_| panel(pal::PANEL2))
            .into()
    };

    let tags_owned = if e.project.tags.is_empty() {
        "—".to_string()
    } else {
        e.project.tags.join(" · ")
    };

    let backend_hint = match e.project.wallpaper_type {
        WallpaperType::Video => "Decoded in walld (ffmpeg)",
        WallpaperType::Scene | WallpaperType::Unknown => "Rendered in walld (WE scene)",
        WallpaperType::Web | WallpaperType::Application => "Not supported yet (web/app)",
    };

    let mon = if app.monitor.is_empty() {
        "all monitors"
    } else {
        app.monitor.as_str()
    };

    let apply_label = if app.busy {
        "STARTING…".to_string()
    } else {
        format!("PLAY · {mon}")
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
    .style(apply_style);

    let desc = {
        let d = e.project.description.replace('\n', " ");
        if d.chars().count() > 220 {
            format!("{}…", d.chars().take(220).collect::<String>())
        } else {
            d
        }
    };

    column![
        preview,
        Space::new().height(14),
        text(&e.project.title).size(20).color(pal::FG),
        Space::new().height(4),
        text(format!(
            "{} · {} · {}",
            e.project.wallpaper_type.as_label(),
            e.source.label(),
            format_size(e.size_bytes)
        ))
        .size(12)
        .color(pal::ACCENT)
        .font(Font::MONOSPACE),
        text(format!("id {}", e.id))
            .size(11)
            .color(pal::MUTE)
            .font(Font::MONOSPACE),
        Space::new().height(8),
        text(desc).size(12).color(pal::DIM),
        Space::new().height(10),
        label_inline("TAGS", &tags_owned),
        label_inline("FILE", &e.project.file),
        label_inline("RUNTIME", backend_hint),
        Space::new().height(Fill),
        apply,
        Space::new().height(6),
        button(
            container(text("STOP").size(12).font(Font::MONOSPACE).color(pal::FG))
                .width(Fill)
                .center_x(Fill)
                .padding(10),
        )
        .on_press(Message::Stop)
        .padding(0)
        .width(Fill)
        .style(stop_style),
        Space::new().height(8),
        text("Enter play · S stop · arrows navigate")
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
    let rt = match app.runtime.backend {
        PlayBackend::Walld => "walld",
        PlayBackend::None => "—",
    };
    let status = if app.runtime.playing {
        format!("PLAYING [{rt}] {}", app.runtime.detail)
    } else {
        "IDLE".into()
    };
    let c = if app.last_ok { pal::DIM } else { pal::ERR };
    container(
        row![
            text(&app.last_msg)
                .size(11)
                .color(c)
                .font(Font::MONOSPACE)
                .width(Fill),
            text(status)
                .size(11)
                .color(if app.runtime.playing {
                    pal::OK
                } else {
                    pal::MUTE
                })
                .font(Font::MONOSPACE),
        ]
        .padding(Padding::from([8, 14])),
    )
    .width(Fill)
    .style(|_| panel(pal::PANEL))
    .into()
}

// ── atoms ──────────────────────────────────────────────────────────────────

fn label(s: &str) -> Element<'_, Message> {
    container(text(s).size(10).color(pal::MUTE).font(Font::MONOSPACE))
        .padding(Padding {
            top: 12.0,
            right: 12.0,
            bottom: 6.0,
            left: 12.0,
        })
        .into()
}

fn label_inline(k: &str, v: &str) -> Element<'static, Message> {
    column![
        text(k.to_string()).size(10).color(pal::MUTE).font(Font::MONOSPACE),
        text(v.to_string()).size(11).color(pal::DIM).font(Font::MONOSPACE),
        Space::new().height(6),
    ]
    .into()
}


/// Wallpaper Engine–style display chooser: aspect glyphs + name/resolution.
fn display_picker(app: &App) -> Element<'_, Message> {
    let mut col = column![].spacing(6).padding(Padding {
        top: 0.0,
        right: 10.0,
        bottom: 0.0,
        left: 10.0,
    });

    col = col.push(display_row(
        "All displays",
        None,
        app.monitor.is_empty(),
        Message::SetMonitor(String::new()),
        1.6,
    ));

    for m in &app.monitors {
        let selected = app.monitor == m.name;
        col = col.push(display_row(
            &m.name,
            Some((m.width, m.height)),
            selected,
            Message::SetMonitor(m.name.clone()),
            m.aspect(),
        ));
    }

    col.into()
}

fn display_row<'a>(
    name: &'a str,
    res: Option<(u32, u32)>,
    on: bool,
    msg: Message,
    aspect: f32,
) -> Element<'a, Message> {
    let glyph_w = 36.0_f32;
    let glyph_h = (glyph_w / aspect.max(0.5)).clamp(14.0, 28.0);
    let glyph = container(Space::new().width(Length::Fixed(glyph_w - 4.0)).height(Length::Fixed(glyph_h - 4.0)))
        .width(Length::Fixed(glyph_w))
        .height(Length::Fixed(glyph_h))
        .center_x(Fill)
        .center_y(Fill)
        .style(move |_| container::Style {
            background: Some(Background::Color(if on {
                Color::from_rgb(0.20, 0.17, 0.12)
            } else {
                pal::PANEL2
            })),
            border: Border {
                color: if on { pal::ACCENT } else { pal::LINE },
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        });

    let sub = match res {
        Some((w, h)) => format!("{w}×{h}"),
        None => "span every output".into(),
    };

    button(
        row![
            glyph,
            Space::new().width(10),
            column![
                text(name)
                    .size(12)
                    .color(if on { pal::FG } else { pal::DIM }),
                text(sub)
                    .size(10)
                    .color(pal::MUTE)
                    .font(Font::MONOSPACE),
            ]
            .spacing(2)
            .width(Fill),
            if on {
                text("✓").size(12).color(pal::ACCENT).font(Font::MONOSPACE)
            } else {
                text(" ").size(12)
            },
        ]
        .align_y(Alignment::Center)
        .padding(Padding::from([6, 4])),
    )
    .on_press(msg)
    .padding(0)
    .width(Fill)
    .style(move |_t, status| {
        let bg = if on {
            Color::from_rgb(0.12, 0.11, 0.09)
        } else if matches!(status, button::Status::Hovered) {
            pal::PANEL2
        } else {
            Color::TRANSPARENT
        };
        button::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: if on { pal::ACCENT } else { Color::TRANSPARENT },
                width: if on { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            text_color: pal::FG,
            shadow: Default::default(),
            snap: true,
        }
    })
    .into()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}

fn filter_btn(label: &str, on: bool, msg: Message) -> Element<'_, Message> {
    let mark = if on { "▣" } else { "□" };
    button(
        row![
            text(mark)
                .size(13)
                .color(if on { pal::ACCENT } else { pal::MUTE })
                .font(Font::MONOSPACE),
            Space::new().width(8),
            text(label)
                .size(13)
                .color(if on { pal::FG } else { pal::DIM }),
        ]
        .align_y(Alignment::Center)
        .padding(Padding::from([7, 12])),
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
                radius: 0.0.into(),
                color: Color::TRANSPARENT,
            },
            text_color: pal::FG,
            shadow: Default::default(),
            snap: true,
        }
    })
    .into()
}

fn flat(label: impl Into<String>, msg: Message) -> Element<'static, Message> {
    let label = label.into();
    button(text(label).size(11).font(Font::MONOSPACE))
        .on_press(msg)
        .padding(Padding::from([6, 10]))
        .style(|_t, status| {
            let (bg, border) = match status {
                button::Status::Hovered => (pal::PANEL2, Color::from_rgb(0.35, 0.35, 0.37)),
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

fn hrule() -> Element<'static, Message> {
    rule::horizontal(1)
        .style(|_| rule::Style {
            color: pal::LINE,
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

fn vrule() -> Element<'static, Message> {
    rule::vertical(1)
        .style(|_| rule::Style {
            color: pal::LINE,
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

fn search_style(_t: &iced::Theme, status: text_input::Status) -> text_input::Style {
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

fn apply_style(_t: &iced::Theme, status: button::Status) -> button::Style {
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

fn stop_style(_t: &iced::Theme, status: button::Status) -> button::Style {
    let border = match status {
        button::Status::Hovered => pal::ERR,
        _ => pal::LINE,
    };
    button::Style {
        background: Some(Background::Color(Color::TRANSPARENT)),
        border: Border {
            color: border,
            width: 1.0,
            radius: 0.0.into(),
        },
        text_color: pal::FG,
        shadow: Default::default(),
        snap: true,
    }
}
