//! Wallpaper Engine–style chrome for wallstudio.

use crate::downloads::{Download, State as DownloadState};
use crate::workshop::{is_subscribed, WorkshopSort};
use crate::{App, Confirm, FitModeUi, MainTab, Message, TypeFilter, WE_AGE_RATINGS, WE_GENRE_TAGS};
use iced::widget::{
    button, checkbox, column, container, image, mouse_area, pick_list, row, rule, scrollable,
    slider, text, text_input, Space,
};
use iced::widget::{canvas, stack};
use iced::{Alignment, Background, Border, Color, Element, Fill, Font, Length, Padding};
use wallengine_we::format_size;
use wallengine_we::PlayBackend;
use wallengine_we::PropKind;
use wallengine_we::PropValue;
use wallengine_we::WallpaperType;

/// Colours come from the live palette (`theme::pal`); the sizes are layout
/// constants that never follow the wallpaper.
pub(crate) mod pal {
    pub use crate::theme::pal::*;
    pub const SIDE: f32 = 200.0;
    pub const DETAIL: f32 = 320.0;
    pub const DETAIL_NARROW: f32 = 260.0;
}

fn side_w(app: &App) -> f32 {
    if app.window_width < 900.0 {
        168.0
    } else {
        pal::SIDE
    }
}

fn detail_w(app: &App) -> f32 {
    if app.window_width < 900.0 {
        pal::DETAIL_NARROW
    } else if app.window_width < 1100.0 {
        300.0
    } else {
        pal::DETAIL
    }
}

pub fn view(app: &App) -> Element<'_, Message> {
    let side = side_w(app);
    let detail_width = detail_w(app);
    let body = match app.tab {
        MainTab::Library => row![
            container(sidebar(app))
                .width(Length::Fixed(side))
                .height(Fill)
                .style(|_| panel(pal::panel())),
            vrule(),
            container(gallery(app))
                .width(Fill)
                .height(Fill)
                .style(|_| panel(pal::bg())),
            vrule(),
            container(detail(app))
                .width(Length::Fixed(detail_width))
                .height(Fill)
                .style(|_| panel(pal::panel())),
        ]
        .height(Fill),
        MainTab::Workshop => row![
            container(workshop_sidebar(app))
                .width(Length::Fixed(side))
                .height(Fill)
                .style(|_| panel(pal::panel())),
            vrule(),
            container(workshop_gallery(app))
                .width(Fill)
                .height(Fill)
                .style(|_| panel(pal::bg())),
            vrule(),
            container(workshop_detail(app))
                .width(Length::Fixed(detail_width))
                .height(Fill)
                .style(|_| panel(pal::panel())),
        ]
        .height(Fill),
        MainTab::Settings => row![crate::ui_settings::view(app)].height(Fill),
    };

    // Status lives in the top bar — no bottom footer (saves vertical space).
    container(column![top(app), hrule(), body].width(Fill).height(Fill))
        .width(Fill)
        .height(Fill)
        .style(|_| panel(pal::bg()))
        .into()
}

fn tab_btn(label: &str, active: bool, msg: Message) -> Element<'_, Message> {
    let fg = if active { pal::on_accent() } else { pal::dim() };
    button(
        container(text(label).size(12).font(Font::MONOSPACE).color(fg))
            .padding(Padding::from([6, 14])),
    )
    .on_press(msg)
    .padding(0)
    .style(move |_t, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(if active {
                pal::accent_fill()
            } else if hover {
                Background::Color(pal::panel2())
            } else {
                Background::Color(Color::TRANSPARENT)
            }),
            text_color: fg,
            border: Border {
                color: if active { pal::accent() } else { pal::line() },
                width: 1.0,
                radius: pal::radius().into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn top(app: &App) -> Element<'_, Message> {
    let brand = row![
        text("WALL").size(15).color(pal::fg()).font(Font::MONOSPACE),
        text("STUDIO")
            .size(15)
            .color(pal::accent())
            .font(Font::MONOSPACE),
    ]
    .align_y(Alignment::Center);

    let tabs = row![
        tab_btn(
            "Library",
            app.tab == MainTab::Library,
            Message::SetTab(MainTab::Library),
        ),
        Space::new().width(4),
        tab_btn(
            "Workshop",
            app.tab == MainTab::Workshop,
            Message::SetTab(MainTab::Workshop),
        ),
        Space::new().width(4),
        tab_btn(
            "⚙ Settings",
            app.tab == MainTab::Settings,
            Message::SetTab(MainTab::Settings),
        ),
    ]
    .align_y(Alignment::Center);

    let search_w = if app.window_width < 1000.0 {
        160.0
    } else {
        240.0
    };
    let search: Element<'_, Message> = match app.tab {
        MainTab::Library => text_input("Filter…", &app.filter)
            .on_input(Message::FilterChanged)
            .on_submit(Message::Apply)
            .padding(6)
            .size(12)
            .width(Length::Fixed(search_w))
            .style(search_style)
            .into(),
        MainTab::Workshop => text_input("Search Workshop…", &app.workshop.search)
            .on_input(Message::WorkshopSearch)
            .on_submit(Message::WorkshopRefresh)
            .padding(6)
            .size(12)
            .width(Length::Fixed(search_w))
            .style(search_style)
            .into(),
        // Nothing to filter on the settings page.
        MainTab::Settings => Space::new().width(Length::Fixed(0.0)).into(),
    };

    let refresh_msg = if app.tab == MainTab::Workshop {
        Message::WorkshopRefresh
    } else {
        Message::Refresh
    };

    let engine_label = match app.engine {
        PlayBackend::Lwe => {
            if app.runtime.lwe_ready {
                "ENGINE  LWE"
            } else {
                "ENGINE  LWE?"
            }
        }
        _ => {
            if app.runtime.engine_ready {
                "ENGINE  walld"
            } else {
                "ENGINE  offline"
            }
        }
    };
    let engine_ok = match app.engine {
        PlayBackend::Lwe => app.runtime.lwe_ready,
        _ => app.runtime.engine_ready,
    };
    let engine_btn = button(
        container(
            text(engine_label)
                .size(11)
                .font(Font::MONOSPACE)
                .color(if engine_ok { pal::ok() } else { pal::err() }),
        )
        .padding(Padding::from([4, 8])),
    )
    .on_press(Message::ToggleEngine)
    .padding(0)
    .style(move |_t, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if hover {
                pal::panel2()
            } else {
                Color::TRANSPARENT
            })),
            text_color: if engine_ok { pal::ok() } else { pal::err() },
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        }
    });

    // Status snippet (replaces bottom footer).
    let status_c = if app.last_ok { pal::mute() } else { pal::err() };
    let download_count = app
        .downloads
        .iter()
        .filter(|d| !matches!(d.state, DownloadState::Failed(_)))
        .count();
    let status = text(if download_count > 0 {
        format!("{} downloads · see Library", download_count)
    } else {
        app.last_msg.clone()
    })
    .size(10)
    .color(status_c)
    .font(Font::MONOSPACE);

    let bar = row![
        brand,
        Space::new().width(12),
        tabs,
        Space::new().width(8),
        search,
        Space::new().width(6),
        flat("Refresh", refresh_msg),
        Space::new().width(6),
        monitor_dropdown_btn(app),
        Space::new().width(Fill),
        status,
        Space::new().width(8),
        engine_btn,
    ]
    .align_y(Alignment::Center)
    .padding(Padding::from([6, 10]));

    let mut col = column![bar].width(Fill);
    if app.monitor_menu_open {
        col = col.push(hrule());
        col = col.push(monitor_dropdown_panel(app));
    }

    container(col)
        .width(Fill)
        .style(|_| panel(pal::panel()))
        .into()
}

/// Compact monitor picker button (top bar, next to Refresh).
fn monitor_dropdown_btn(app: &App) -> Element<'_, Message> {
    let label = if app.monitor.is_empty() {
        "All displays".to_string()
    } else {
        app.monitor.clone()
    };
    let chevron = if app.monitor_menu_open { "▴" } else { "▾" };
    // Tiny preview of wallpaper currently on this monitor.
    let thumb: Element<'_, Message> = if let Some(e) = app.entry_on_monitor(&app.monitor) {
        if let Some(ref p) = e.preview {
            container(
                image(app.preview_handle(p, true))
                    .width(Length::Fixed(36.0))
                    .height(Length::Fixed(22.0))
                    .content_fit(iced::ContentFit::Cover),
            )
            .width(Length::Fixed(36.0))
            .height(Length::Fixed(22.0))
            .style(|_| container::Style {
                border: Border {
                    color: pal::line(),
                    width: 1.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            })
            .into()
        } else {
            Space::new()
                .width(Length::Fixed(36.0))
                .height(Length::Fixed(22.0))
                .into()
        }
    } else {
        container(
            Space::new()
                .width(Length::Fixed(34.0))
                .height(Length::Fixed(20.0)),
        )
        .width(Length::Fixed(36.0))
        .height(Length::Fixed(22.0))
        .style(|_| container::Style {
            background: Some(Background::Color(pal::panel2())),
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
    };

    button(
        row![
            thumb,
            Space::new().width(6),
            text(label).size(11).color(pal::fg()).font(Font::MONOSPACE),
            Space::new().width(4),
            text(chevron).size(10).color(pal::mute()),
        ]
        .align_y(Alignment::Center)
        .padding(Padding::from([3, 6])),
    )
    .on_press(Message::ToggleMonitorMenu)
    .padding(0)
    .style(|_t, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if hover {
                pal::panel2()
            } else {
                pal::bg()
            })),
            text_color: pal::fg(),
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        }
    })
    .into()
}

fn monitor_dropdown_panel(app: &App) -> Element<'_, Message> {
    let mut col = column![text("DISPLAY")
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE)]
    .spacing(4)
    .padding(Padding::from([8, 12]));

    col = col.push(monitor_menu_row(
        app,
        "All displays",
        "",
        None,
        app.monitor.is_empty(),
        Message::SetMonitor(String::new()),
    ));
    for m in &app.monitors {
        let selected = app.monitor == m.name;
        col = col.push(monitor_menu_row(
            app,
            &m.name,
            &m.name,
            Some((m.width, m.height)),
            selected,
            Message::SetMonitor(m.name.clone()),
        ));
    }
    container(col)
        .width(Fill)
        .style(|_| panel(pal::panel2()))
        .into()
}

fn monitor_menu_row<'a>(
    app: &'a App,
    title: &'a str,
    mon_key: &'a str,
    res: Option<(u32, u32)>,
    on: bool,
    msg: Message,
) -> Element<'a, Message> {
    let thumb: Element<'a, Message> = if let Some(e) = app.entry_on_monitor(mon_key) {
        if let Some(ref p) = e.preview {
            container(
                image(app.preview_handle(p, true))
                    .width(Length::Fixed(48.0))
                    .height(Length::Fixed(28.0))
                    .content_fit(iced::ContentFit::Cover),
            )
            .width(Length::Fixed(48.0))
            .height(Length::Fixed(28.0))
            .style(move |_| {
                let border_c = if on { pal::accent() } else { pal::line() };
                container::Style {
                    border: Border {
                        color: border_c,
                        width: 1.0,
                        radius: 0.0.into(),
                    },
                    ..Default::default()
                }
            })
            .into()
        } else {
            empty_thumb(on)
        }
    } else {
        empty_thumb(on)
    };

    let sub = match res {
        Some((w, h)) => format!("{w}×{h}"),
        None => "every output".into(),
    };
    let playing_title = app
        .entry_on_monitor(mon_key)
        .map(|e| e.project.title.as_str())
        .unwrap_or("—");

    button(
        row![
            thumb,
            Space::new().width(10),
            column![
                text(title)
                    .size(12)
                    .color(if on { pal::fg() } else { pal::dim() }),
                text(format!("{sub} · {playing_title}"))
                    .size(10)
                    .color(pal::mute())
                    .font(Font::MONOSPACE),
            ]
            .spacing(2)
            .width(Fill),
            if on {
                text("✓")
                    .size(12)
                    .color(pal::accent())
                    .font(Font::MONOSPACE)
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
            pal::accent_bg()
        } else if matches!(status, button::Status::Hovered) {
            pal::panel2()
        } else {
            Color::TRANSPARENT
        };
        button::Style {
            background: Some(Background::Color(bg)),
            border: Border {
                color: if on {
                    pal::accent()
                } else {
                    Color::TRANSPARENT
                },
                width: if on { 1.0 } else { 0.0 },
                radius: 0.0.into(),
            },
            text_color: pal::fg(),
            shadow: Default::default(),
            snap: true,
        }
    })
    .into()
}

fn empty_thumb(on: bool) -> Element<'static, Message> {
    let border_c = if on { pal::accent() } else { pal::line() };
    container(
        Space::new()
            .width(Length::Fixed(46.0))
            .height(Length::Fixed(26.0)),
    )
    .width(Length::Fixed(48.0))
    .height(Length::Fixed(28.0))
    .style(move |_| container::Style {
        background: Some(Background::Color(pal::panel2())),
        border: Border {
            color: border_c,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    })
    .into()
}

fn sidebar(app: &App) -> Element<'_, Message> {
    let genre_counts = app.genre_counts();
    let rating_counts = app.rating_counts();
    let genre_active = !app.filter_genres.is_empty();
    let rating_active = !app.filter_ratings.is_empty();

    let genre_clear: Element<'_, Message> = if genre_active {
        button(text("Clear").size(10).color(pal::accent()))
            .padding(Padding::from([2, 6]))
            .style(|_t: &iced::Theme, s| filter_clear_style(s))
            .on_press(Message::ClearGenres)
            .into()
    } else {
        Space::new().width(Length::Fixed(0.0)).into()
    };

    let mut genre_col = column![
        row![label("GENRE"), Space::new().width(Fill), genre_clear].align_y(Alignment::Center),
        text(if genre_active {
            format!(
                "{} selected · from WE project tags",
                app.filter_genres.len()
            )
        } else {
            "All genres (WE tags)".into()
        })
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(4),
    ]
    .spacing(2)
    .width(Fill);

    for &tag in WE_GENRE_TAGS {
        let n = genre_counts.get(tag).copied().unwrap_or(0);
        let on = app.filter_genres.contains(tag);
        genre_col = genre_col.push(genre_btn(tag, n, on, Message::ToggleGenre(tag.into())));
    }

    let rating_clear: Element<'_, Message> = if rating_active {
        button(text("Clear").size(10).color(pal::accent()))
            .padding(Padding::from([2, 6]))
            .style(|_t: &iced::Theme, s| filter_clear_style(s))
            .on_press(Message::ClearRatings)
            .into()
    } else {
        Space::new().width(Length::Fixed(0.0)).into()
    };

    let mut rating_col = column![
        row![label("AGE RATING"), Space::new().width(Fill), rating_clear]
            .align_y(Alignment::Center),
        text(if rating_active {
            format!("{} selected · contentrating", app.filter_ratings.len())
        } else {
            "All ratings".into()
        })
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(4),
    ]
    .spacing(2)
    .width(Fill);

    for &rating in WE_AGE_RATINGS {
        let n = rating_counts.get(rating).copied().unwrap_or(0);
        let on = app.filter_ratings.contains(rating);
        rating_col = rating_col.push(genre_btn(
            rating,
            n,
            on,
            Message::ToggleRating(rating.into()),
        ));
    }

    // Display picker lives in the top bar (next to Refresh).
    let body = column![
        label("TYPE"),
        filter_btn(
            "All",
            app.filter_type == TypeFilter::All,
            Message::SetTypeFilter(TypeFilter::All)
        ),
        filter_btn(
            "Scene (.pkg)",
            app.filter_type == TypeFilter::Scene,
            Message::SetTypeFilter(TypeFilter::Scene)
        ),
        filter_btn(
            "Video",
            app.filter_type == TypeFilter::Video,
            Message::SetTypeFilter(TypeFilter::Video)
        ),
        filter_btn(
            "Web",
            app.filter_type == TypeFilter::Web,
            Message::SetTypeFilter(TypeFilter::Web)
        ),
        Space::new().height(10),
        label("SOURCE"),
        filter_btn(
            "Steam (subscribed)",
            app.filter_source_workshop,
            Message::ToggleWorkshop
        ),
        filter_btn("My projects", app.filter_source_local, Message::ToggleLocal),
        Space::new().height(10),
        rating_col,
        Space::new().height(10),
        genre_col,
        Space::new().height(10),
        label("OPTIONS"),
        filter_btn(
            if app.silent {
                "Audio muted"
            } else {
                "Audio on"
            },
            !app.silent,
            Message::ToggleSilent,
        ),
        filter_btn(
            if app.sort_newest {
                "Sort: newest id"
            } else {
                "Sort: name"
            },
            true,
            Message::ToggleSort,
        ),
        Space::new().height(12),
        label("LIBRARY"),
        text(format!(
            "{} items · {} shown",
            app.entries.len(),
            app.visible().len()
        ))
        .size(11)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(6),
        flat("Open folder", Message::OpenFolder),
        Space::new().height(8),
    ]
    .width(Fill)
    .padding(Padding::from([6, 0]));

    scrollable(body).height(Fill).width(Fill).into()
}

fn genre_btn(tag: &str, count: usize, on: bool, msg: Message) -> Element<'_, Message> {
    let label = if count > 0 {
        format!("{tag}  ({count})")
    } else {
        tag.to_string()
    };
    let fg = if on {
        pal::accent()
    } else if count == 0 {
        pal::mute()
    } else {
        pal::dim()
    };
    button(
        container(
            row![
                text(if on { "●" } else { "○" })
                    .size(10)
                    .color(if on { pal::accent() } else { pal::mute() })
                    .font(Font::MONOSPACE),
                Space::new().width(6),
                text(label).size(12).color(fg),
            ]
            .align_y(Alignment::Center),
        )
        .width(Fill)
        .padding(Padding::from([5, 10])),
    )
    .on_press(msg)
    .padding(0)
    .width(Fill)
    .style(move |_t, status| genre_btn_style(on, status))
    .into()
}

pub(crate) fn genre_btn_style(on: bool, status: button::Status) -> button::Style {
    let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if on {
            crate::theme::lighten(pal::accent_bg(), 0.04)
        } else if hover {
            pal::panel2()
        } else {
            Color::TRANSPARENT
        })),
        text_color: pal::fg(),
        border: Border {
            color: if on {
                pal::accent_lo()
            } else {
                Color::TRANSPARENT
            },
            width: if on { 1.0 } else { 0.0 },
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn filter_clear_style(status: button::Status) -> button::Style {
    let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hover {
            pal::panel2()
        } else {
            Color::TRANSPARENT
        })),
        text_color: pal::accent(),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

fn workshop_sidebar(app: &App) -> Element<'_, Message> {
    let ws = &app.workshop;
    let genre_active = !ws.filter_genres.is_empty();
    let rating_active = !ws.filter_ratings.is_empty();

    let genre_clear: Element<'_, Message> = if genre_active {
        button(text("Clear").size(10).color(pal::accent()))
            .padding(Padding::from([2, 6]))
            .style(|_t: &iced::Theme, s| filter_clear_style(s))
            .on_press(Message::WorkshopClearGenres)
            .into()
    } else {
        Space::new().width(Length::Fixed(0.0)).into()
    };

    let mut genre_col = column![
        row![label("GENRE"), Space::new().width(Fill), genre_clear].align_y(Alignment::Center),
        text(if genre_active {
            format!("{} selected · Steam tags", ws.filter_genres.len())
        } else {
            "All genres".into()
        })
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(4),
    ]
    .spacing(2)
    .width(Fill);

    for &tag in WE_GENRE_TAGS {
        if tag == "Unspecified" {
            continue;
        }
        let on = ws.filter_genres.contains(tag);
        genre_col = genre_col.push(filter_btn(
            tag,
            on,
            Message::WorkshopToggleGenre(tag.into()),
        ));
    }

    let rating_clear: Element<'_, Message> = if rating_active {
        button(text("Clear").size(10).color(pal::accent()))
            .padding(Padding::from([2, 6]))
            .style(|_t: &iced::Theme, s| filter_clear_style(s))
            .on_press(Message::WorkshopClearRatings)
            .into()
    } else {
        Space::new().width(Length::Fixed(0.0)).into()
    };

    let mut rating_col = column![
        row![label("AGE RATING"), Space::new().width(Fill), rating_clear]
            .align_y(Alignment::Center),
        text(if rating_active {
            format!("{} selected", ws.filter_ratings.len())
        } else {
            "All ratings".into()
        })
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(4),
    ]
    .spacing(2)
    .width(Fill);

    for &rating in WE_AGE_RATINGS {
        let on = ws.filter_ratings.contains(rating);
        rating_col = rating_col.push(filter_btn(
            rating,
            on,
            Message::WorkshopToggleRating(rating.into()),
        ));
    }

    let mut sort_col = column![label("SORT")].spacing(2).width(Fill);
    for s in WorkshopSort::ALL {
        // Hide pure "Relevance" unless searching — Trend becomes textsearch on query.
        if matches!(s, WorkshopSort::TextSearch) && ws.search.trim().is_empty() {
            continue;
        }
        sort_col = sort_col.push(filter_btn(
            s.label(),
            ws.sort == s
                || (matches!(s, WorkshopSort::TextSearch)
                    && !ws.search.trim().is_empty()
                    && matches!(ws.sort, WorkshopSort::Trend | WorkshopSort::TextSearch)),
            Message::WorkshopSetSort(s),
        ));
    }

    let body = column![
        label("STEAM WORKSHOP"),
        text("Wallpaper Engine · 431960")
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        Space::new().height(8),
        sort_col,
        Space::new().height(12),
        label("TYPE"),
        filter_btn(
            "All",
            ws.filter_type == TypeFilter::All,
            Message::WorkshopSetType(TypeFilter::All),
        ),
        filter_btn(
            "Scene",
            ws.filter_type == TypeFilter::Scene,
            Message::WorkshopSetType(TypeFilter::Scene),
        ),
        filter_btn(
            "Video",
            ws.filter_type == TypeFilter::Video,
            Message::WorkshopSetType(TypeFilter::Video),
        ),
        filter_btn(
            "Web",
            ws.filter_type == TypeFilter::Web,
            Message::WorkshopSetType(TypeFilter::Web),
        ),
        Space::new().height(12),
        rating_col,
        Space::new().height(12),
        genre_col,
        Space::new().height(16),
        label("PAGE"),
        row![
            flat("◀ Prev", Message::WorkshopPage(-1)),
            Space::new().width(6),
            text(format!("{}", ws.page))
                .size(13)
                .color(pal::accent())
                .font(Font::MONOSPACE),
            Space::new().width(6),
            flat("Next ▶", Message::WorkshopPage(1)),
        ]
        .align_y(Alignment::Center),
        Space::new().height(8),
        text(if ws.loading {
            "Loading…".into()
        } else {
            format!("{} on page", ws.items.len())
        })
        .size(11)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(12),
        text("Uses your Steam login\n(same as sub/unsub).\n50 items / page · full\nWE workshop catalog.")
            .size(10)
            .color(pal::mute()),
        Space::new().height(6),
        text("Keep Steam running.\nLibrary auto-updates\nwhen downloads finish.")
            .size(9)
            .color(pal::mute()),
    ]
    .width(Fill)
    .padding(Padding::from([8, 0]));

    scrollable(body).height(Fill).width(Fill).into()
}

fn workshop_gallery(app: &App) -> Element<'_, Message> {
    let ws = &app.workshop;
    let (cols, cell_w, thumb_h) = app.grid_metrics();
    let thumb_w = (cell_w - 8.0).max(120.0);

    if ws.loading && ws.items.is_empty() {
        return container(
            column![
                text("Loading Steam Workshop…")
                    .size(18)
                    .color(pal::accent()),
                text(format!(
                    "Page {} · titles first, previews stream in",
                    ws.page
                ))
                .size(13)
                .color(pal::mute()),
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

    if let Some(err) = &ws.error {
        if ws.items.is_empty() {
            return container(
                column![
                    text("Workshop unavailable").size(18).color(pal::err()),
                    text(err).size(12).color(pal::mute()),
                    Space::new().height(12),
                    flat("Retry", Message::WorkshopRefresh),
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
    }

    if ws.items.is_empty() {
        return container(
            column![
                text("No workshop items").size(18).color(pal::dim()),
                text("Try another sort, clear filters, or search.")
                    .size(13)
                    .color(pal::mute()),
                Space::new().height(8),
                flat("Reload page 1", Message::WorkshopRefresh),
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
    let idxs: Vec<usize> = (0..ws.items.len()).collect();
    for chunk in idxs.chunks(cols) {
        let mut r = row![].spacing(gap);
        for &idx in chunk {
            r = r.push(workshop_tile(app, idx, thumb_w, thumb_h, cell_w));
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

fn workshop_tile(
    app: &App,
    idx: usize,
    thumb_w: f32,
    thumb_h: f32,
    cell_w: f32,
) -> Element<'_, Message> {
    let item = &app.workshop.items[idx];
    let selected = idx == app.workshop.cursor;
    let download = app.downloads.iter().find(|d| d.item.id == item.id);
    let installed = download.is_none() && is_subscribed(&item.id);

    let img_w = (thumb_w - 2.0).max(80.0);
    let img_h = (thumb_h - 2.0).max(50.0);

    let preview: Element<'_, Message> = if let Some(ref p) = item.preview_path {
        // Prefer animated GIF frames when Steam served a GIF preview.
        container(
            image(app.preview_handle(p, selected))
                .width(Length::Fixed(img_w))
                .height(Length::Fixed(img_h))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Length::Fixed(img_w))
        .height(Length::Fixed(img_h))
        .into()
    } else {
        container(
            text(item.media_type_label())
                .size(14)
                .color(pal::mute())
                .font(Font::MONOSPACE),
        )
        .width(Length::Fixed(img_w))
        .height(Length::Fixed(img_h))
        .center_x(Fill)
        .center_y(Fill)
        .style(|_| panel(pal::panel2()))
        .into()
    };

    let preview = if let Some(d) = download {
        download_overlay(app, preview, d)
    } else {
        preview
    };
    let media = container(preview).style(|_| container::Style {
        background: Some(Background::Color(pal::panel2())),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    let max_chars = ((thumb_w / 6.5) as usize).clamp(12, 48);
    let title = truncate_chars(&item.title, max_chars);
    let caption = column![
        text(title)
            .size(12)
            .color(if selected { pal::fg() } else { pal::dim() }),
        row![
            text(item.media_type_label())
                .size(10)
                .color(pal::accent())
                .font(Font::MONOSPACE),
            Space::new().width(Fill),
            text(format_subs(item.subscriptions))
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE),
            if installed {
                text("  ●").size(10).color(pal::ok())
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

    let cell = container(
        column![media, caption]
            .spacing(0)
            .width(Length::Fixed(thumb_w)),
    )
    .width(Length::Fixed(cell_w))
    .padding(4)
    .style(move |_| container::Style {
        background: Some(if selected {
            pal::accent_bg_fill()
        } else {
            Background::Color(Color::TRANSPARENT)
        }),
        border: Border {
            color: if selected {
                pal::accent()
            } else if installed {
                pal::ok()
            } else {
                Color::TRANSPARENT
            },
            width: if selected || installed { 1.5 } else { 0.0 },
            radius: 0.0.into(),
        },
        ..Default::default()
    });

    mouse_area(cell)
        .on_press(Message::WorkshopSelect(idx))
        .on_double_click(if installed {
            Message::WorkshopPlayInstalled
        } else {
            Message::WorkshopSubscribe
        })
        .into()
}

fn workshop_detail(app: &App) -> Element<'_, Message> {
    let Some(item) = app.workshop.selected() else {
        return container(
            text(if app.workshop.loading {
                "Loading workshop…"
            } else {
                "Select a workshop wallpaper"
            })
            .size(14)
            .color(pal::dim()),
        )
        .padding(20)
        .into();
    };

    let download = app.downloads.iter().find(|d| d.item.id == item.id);
    let installed = download.is_none() && is_subscribed(&item.id);

    let preview: Element<'_, Message> = if let Some(ref p) = item.preview_path {
        container(
            image(app.preview_handle(p, true))
                .width(Fill)
                .height(Length::Fixed(180.0))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Fill)
        .height(Length::Fixed(180.0))
        .style(|_| container::Style {
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
    } else {
        container(text("No preview").size(12).color(pal::mute()))
            .width(Fill)
            .height(Length::Fixed(180.0))
            .center_x(Fill)
            .center_y(Fill)
            .style(|_| panel(pal::panel2()))
            .into()
    };

    let genres = {
        let g = item.genre_tags();
        if g.is_empty() {
            "—".to_string()
        } else {
            g.join(" · ")
        }
    };

    let desc = {
        let d = item.description.replace('\n', " ");
        if d.chars().count() > 280 {
            format!("{}…", d.chars().take(280).collect::<String>())
        } else if d.is_empty() {
            "No description.".into()
        } else {
            d
        }
    };

    let status_line = if let Some(d) = download {
        d.state.label()
    } else if installed {
        "INSTALLED · ready to play"
    } else if app.workshop.loading {
        "Loading…"
    } else {
        "Not subscribed · Subscribe downloads via Steam"
    };

    let primary = if download.is_some_and(|d| !matches!(d.state, DownloadState::Failed(_))) {
        button(
            container(text(status_line).size(13))
                .padding(14)
                .center_x(Fill),
        )
        .width(Fill)
        .padding(0)
        .style(stop_style)
    } else if installed {
        button(
            container(
                text(if app.busy {
                    "STARTING…".to_string()
                } else {
                    "PLAY".into()
                })
                .size(13)
                .font(Font::MONOSPACE)
                .color(pal::on_accent()),
            )
            .width(Fill)
            .center_x(Fill)
            .padding(14),
        )
        .on_press(Message::WorkshopPlayInstalled)
        .padding(0)
        .width(Fill)
        .style(apply_style)
    } else {
        button(
            container(
                text(if download.is_some() {
                    "RETRY DOWNLOAD"
                } else {
                    "SUBSCRIBE"
                })
                .size(13)
                .font(Font::MONOSPACE)
                .color(pal::on_accent()),
            )
            .width(Fill)
            .center_x(Fill)
            .padding(14),
        )
        .on_press(Message::WorkshopSubscribe)
        .padding(0)
        .width(Fill)
        .style(apply_style)
    };

    let unsub_btn: Element<'_, Message> =
        if installed || download.is_some_and(|d| d.state != DownloadState::Subscribing) {
            danger_btn(
                "UNSUBSCRIBE",
                app.is_armed(&Confirm::WorkshopUnsubscribe(
                    item.id.clone(),
                    item.title.clone(),
                )),
                Message::WorkshopUnsubscribe,
            )
        } else {
            Space::new().height(0).into()
        };

    let open_steam = button(
        container(
            text("OPEN IN STEAM")
                .size(12)
                .font(Font::MONOSPACE)
                .color(pal::fg()),
        )
        .width(Fill)
        .center_x(Fill)
        .padding(10),
    )
    .on_press(Message::WorkshopOpenSteam)
    .padding(0)
    .width(Fill)
    .style(stop_style);

    let open_web = button(
        container(
            text("OPEN IN BROWSER")
                .size(12)
                .font(Font::MONOSPACE)
                .color(pal::fg()),
        )
        .width(Fill)
        .center_x(Fill)
        .padding(10),
    )
    .on_press(Message::WorkshopOpenWeb)
    .padding(0)
    .width(Fill)
    .style(stop_style);

    let header = column![
        preview,
        Space::new().height(14),
        text(&item.title).size(20).color(pal::fg()),
        Space::new().height(4),
        text(format!(
            "{} · {}",
            item.media_type_label(),
            format_size(item.file_size)
        ))
        .size(12)
        .color(pal::accent())
        .font(Font::MONOSPACE),
        text(format!("id {}", item.id))
            .size(11)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        Space::new().height(6),
        text(status_line)
            .size(11)
            .color(if installed { pal::ok() } else { pal::dim() })
            .font(Font::MONOSPACE),
        Space::new().height(8),
        if let Some(d) = download {
            column![
                download_status(app, d),
                text(match &d.state {
                    DownloadState::Failed(error) => error.as_str(),
                    _ => "Steam does not expose an exact percentage here. Open Steam to manage downloads.",
                }).size(11).color(pal::dim()),
            ].spacing(8)
        } else { column![] },
        text(desc).size(12).color(pal::dim()),
        Space::new().height(10),
        label_inline("RATING", item.content_rating()),
        label_inline("TAGS", &genres),
        label_inline("SUBS", &format_subs(item.subscriptions)),
        label_inline("FAVS", &format_subs(item.favorited)),
        label_inline("VIEWS", &format_subs(item.views)),
    ]
    .width(Fill);

    column![
        header,
        Space::new().height(Fill),
        primary,
        Space::new().height(6),
        unsub_btn,
        if installed {
            Space::new().height(6)
        } else {
            Space::new().height(0)
        },
        open_steam,
        Space::new().height(6),
        open_web,
        Space::new().height(8),
        text("Enter subscribe/play · U unsub · [ ] page · arrows")
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
    ]
    .padding(14)
    .width(Fill)
    .height(Fill)
    .into()
}

fn format_subs(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// An indeterminate ring is intentional: Steam preallocates its files, so their
/// lengths cannot safely be converted into a download percentage.
struct DownloadRing {
    angle: f32,
    failed: bool,
}

impl canvas::Program<Message> for DownloadRing {
    type State = ();
    fn draw(
        &self,
        _: &(),
        renderer: &iced::Renderer,
        _: &iced::Theme,
        bounds: iced::Rectangle,
        _: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = frame.center();
        let radius = bounds.width.min(bounds.height) / 2.0 - 3.0;
        frame.stroke(
            &canvas::Path::circle(center, radius),
            canvas::Stroke::default()
                .with_width(3.0)
                .with_color(pal::line()),
        );
        let arc = canvas::Path::new(|p| {
            p.arc(canvas::path::Arc {
                center,
                radius,
                start_angle: iced::Radians(self.angle),
                end_angle: iced::Radians(self.angle + std::f32::consts::PI * 1.4),
            })
        });
        frame.stroke(
            &arc,
            canvas::Stroke::default()
                .with_width(3.0)
                .with_color(if self.failed {
                    pal::err()
                } else {
                    pal::accent()
                }),
        );
        vec![frame.into_geometry()]
    }
}

fn download_status<'a>(app: &App, d: &'a Download) -> Element<'a, Message> {
    row![
        canvas(DownloadRing {
            angle: if d.state.animates() {
                app.download_animation
            } else {
                -std::f32::consts::FRAC_PI_2
            },
            failed: matches!(d.state, DownloadState::Failed(_)),
        })
        .width(30)
        .height(30),
        text(d.state.label()).size(11).color(pal::fg()),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .into()
}

fn download_overlay<'a>(
    app: &App,
    preview: Element<'a, Message>,
    d: &'a Download,
) -> Element<'a, Message> {
    stack![
        preview,
        container(download_status(app, d))
            .width(Fill)
            .height(Fill)
            .center_x(Fill)
            .center_y(Fill)
            .style(|_| panel(Color::from_rgba(0.02, 0.03, 0.04, 0.80)))
    ]
    .into()
}

fn download_tile<'a>(
    app: &App,
    d: &'a Download,
    thumb_w: f32,
    thumb_h: f32,
    cell_w: f32,
) -> Element<'a, Message> {
    let preview: Element<'a, Message> = if let Some(path) = &d.item.preview_path {
        image(iced::widget::image::Handle::from_path(path.clone()))
            .width(thumb_w)
            .height(thumb_h)
            .content_fit(iced::ContentFit::Cover)
            .into()
    } else {
        container(Space::new())
            .width(thumb_w)
            .height(thumb_h)
            .style(|_| panel(pal::panel2()))
            .into()
    };
    let tile = column![
        download_overlay(app, preview, d),
        text(truncate_chars(
            &d.item.title,
            ((thumb_w / 6.5) as usize).clamp(12, 48)
        ))
        .size(12)
        .color(pal::fg()),
        text(format!(
            "{} · {}",
            d.item.media_type_label(),
            format_size(d.item.file_size)
        ))
        .size(10)
        .color(pal::dim()),
    ]
    .spacing(6)
    .width(thumb_w);
    button(container(tile).padding(4).width(cell_w))
        .on_press(Message::ShowDownload(d.item.id.clone()))
        .padding(0)
        .style(stop_style)
        .into()
}

fn gallery(app: &App) -> Element<'_, Message> {
    let vis = app.visible();
    let (cols, cell_w, thumb_h) = app.grid_metrics();
    let thumb_w = (cell_w - 8.0).max(120.0);
    if vis.is_empty() && app.downloads.is_empty() {
        return container(
            column![
                text("No wallpapers found").size(18).color(pal::dim()),
                text("Open the Workshop tab to browse & subscribe.")
                    .size(13)
                    .color(pal::mute()),
                text(wallengine_we::workshop_dir().display().to_string())
                    .size(11)
                    .color(pal::mute())
                    .font(Font::MONOSPACE),
                Space::new().height(12),
                flat("Open Workshop", Message::SetTab(MainTab::Workshop)),
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
    if !app.downloads.is_empty() {
        rows = rows.push(
            text(format!("Downloads · {}", app.downloads.len()))
                .size(14)
                .color(pal::fg()),
        );
        for chunk in app.downloads.chunks(cols) {
            let mut r = row![].spacing(gap);
            for d in chunk {
                r = r.push(download_tile(app, d, thumb_w, thumb_h, cell_w));
            }
            rows = rows.push(r);
        }
        if !vis.is_empty() {
            rows = rows.push(text("Installed").size(14).color(pal::dim()));
        }
    }
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
    let playing =
        app.runtime.playing && (app.runtime.title == e.id || app.runtime.detail.contains(&e.id));

    let img_w = (thumb_w - 2.0).max(80.0);
    let img_h = (thumb_h - 2.0).max(50.0);

    let preview: Element<'_, Message> = if let Some(ref p) = e.preview {
        container(
            image(app.preview_handle(p, selected))
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
                .color(pal::mute())
                .font(Font::MONOSPACE),
        )
        .width(Length::Fixed(img_w))
        .height(Length::Fixed(img_h))
        .center_x(Fill)
        .center_y(Fill)
        .style(|_| panel(pal::panel2()))
        .into()
    };

    let media = container(preview).style(|_| container::Style {
        background: Some(Background::Color(pal::panel2())),
        border: Border {
            color: pal::line(),
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
            .color(if selected { pal::fg() } else { pal::dim() }),
        row![
            text(ty).size(10).color(pal::accent()).font(Font::MONOSPACE),
            Space::new().width(Fill),
            text(format_size(e.size_bytes))
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE),
            if playing {
                text("  ●").size(10).color(pal::ok())
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

    let cell = container(
        column![media, caption]
            .spacing(0)
            .width(Length::Fixed(thumb_w)),
    )
    .width(Length::Fixed(cell_w))
    .padding(4)
    .style(move |_| container::Style {
        background: Some(if selected {
            pal::accent_bg_fill()
        } else {
            Background::Color(Color::TRANSPARENT)
        }),
        border: Border {
            color: if selected {
                pal::accent()
            } else if playing {
                pal::ok()
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
        return container(text("Select a wallpaper").size(14).color(pal::dim()))
            .padding(16)
            .into();
    };

    // Thumbnail pinned at top (does not scroll away).
    let preview_h = if app.window_height < 560.0 {
        120.0
    } else {
        160.0
    };
    let preview: Element<'_, Message> = if let Some(ref p) = e.preview {
        container(
            image(app.preview_handle(p, true))
                .width(Fill)
                .height(Length::Fixed(preview_h))
                .content_fit(iced::ContentFit::Cover),
        )
        .width(Fill)
        .height(Length::Fixed(preview_h))
        .style(|_| container::Style {
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
    } else {
        container(text("No preview").size(12).color(pal::mute()))
            .width(Fill)
            .height(Length::Fixed(preview_h))
            .center_x(Fill)
            .center_y(Fill)
            .style(|_| panel(pal::panel2()))
            .into()
    };

    let tags_owned = if e.project.tags.is_empty() {
        "—".to_string()
    } else {
        e.project.tags.join(" · ")
    };
    let rating_owned = e
        .project
        .content_rating
        .clone()
        .unwrap_or_else(|| "Everyone".into());

    let backend_hint = match e.project.wallpaper_type {
        WallpaperType::Video => "Decoded in walld (ffmpeg)",
        WallpaperType::Scene | WallpaperType::Unknown => "Rendered in walld (WE scene)",
        WallpaperType::Web => "Chromium web rendering (experimental)",
        WallpaperType::Application => "Application wallpapers are not supported",
    };

    let mon = if app.monitor.is_empty() {
        "all"
    } else {
        app.monitor.as_str()
    };

    let apply_label = if app.busy {
        "STARTING…".to_string()
    } else {
        format!("PLAY · {mon}")
    };

    // Split play button: main action + monitor dropdown caret.
    let apply_main = button(
        container(
            text(apply_label)
                .size(12)
                .font(Font::MONOSPACE)
                .color(pal::on_accent()),
        )
        .width(Fill)
        .center_x(Fill)
        .padding(Padding::from([12, 8])),
    )
    .on_press(Message::Apply)
    .padding(0)
    .width(Fill)
    .style(apply_style);

    let apply_caret = button(
        container(
            text(if app.play_monitor_menu_open {
                "▴"
            } else {
                "▾"
            })
            .size(12)
            .color(pal::on_accent()),
        )
        .center_x(Fill)
        .center_y(Fill)
        .padding(Padding::from([12, 10])),
    )
    .on_press(Message::TogglePlayMonitorMenu)
    .padding(0)
    .style(apply_style);

    let mut apply_col = column![row![apply_main, apply_caret].spacing(1).width(Fill)].width(Fill);
    if app.play_monitor_menu_open {
        apply_col = apply_col.push(play_monitor_menu(app));
    }

    let desc = {
        let d = e.project.description.replace('\n', " ");
        if d.chars().count() > 180 {
            format!("{}…", d.chars().take(180).collect::<String>())
        } else {
            d
        }
    };

    let is_workshop = e.source == wallengine_we::WeSource::Workshop;
    let unsub: Element<'_, Message> = if is_workshop {
        danger_btn(
            "UNSUBSCRIBE",
            app.is_armed(&Confirm::LibraryUnsubscribe(
                e.id.clone(),
                e.project.title.clone(),
            )),
            Message::LibraryUnsubscribe,
        )
    } else {
        Space::new().height(0).into()
    };

    let can_edit = wallengine_we::is_editable_scene(&e.dir, e.project.wallpaper_type);
    let is_local = wallengine_we::is_local_project(&e.dir);
    let edit_label = if is_local && e.dir.join("scene.json").is_file() && !e.has_scene_pkg {
        "EDIT SCENE"
    } else {
        "EDIT SCENE · fork"
    };
    let edit_btn: Element<'_, Message> = if can_edit {
        button(
            container(
                text(if app.busy {
                    "FORKING…".to_string()
                } else {
                    edit_label.to_string()
                })
                .size(11)
                .font(Font::MONOSPACE)
                .color(pal::bg()),
            )
            .width(Fill)
            .center_x(Fill)
            .padding(8),
        )
        .on_press(Message::EditScene)
        .padding(0)
        .width(Fill)
        .style(edit_style)
        .into()
    } else {
        container(
            text("Editor: Scene packages only")
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE),
        )
        .width(Fill)
        .center_x(Fill)
        .padding(6)
        .into()
    };

    let local_mgmt: Element<'_, Message> = if is_local {
        column![
            Space::new().height(6),
            text("LOCAL PROJECT")
                .size(10)
                .color(pal::accent())
                .font(Font::MONOSPACE),
            text_input("name in library…", &app.rename_draft)
                .on_input(Message::RenameDraft)
                .on_submit(Message::RenameProject)
                .padding(6)
                .size(12),
            Space::new().height(4),
            button(
                container(
                    text("RENAME")
                        .size(11)
                        .font(Font::MONOSPACE)
                        .color(pal::fg())
                )
                .width(Fill)
                .center_x(Fill)
                .padding(6),
            )
            .on_press(Message::RenameProject)
            .padding(0)
            .width(Fill)
            .style(stop_style),
            Space::new().height(4),
            danger_btn(
                "DELETE PROJECT",
                app.is_armed(&Confirm::DeleteProject(
                    e.dir.clone(),
                    e.project.title.clone(),
                )),
                Message::DeleteProject,
            ),
        ]
        .spacing(2)
        .width(Fill)
        .into()
    } else {
        Space::new().height(0).into()
    };

    // Scrollable body under pinned thumbnail: info + settings + actions.
    // Settings are long (not nested-scrollable); the whole panel scrolls.
    let scroll_body = column![
        Space::new().height(10),
        text(&e.project.title).size(18).color(pal::fg()),
        Space::new().height(3),
        text(format!(
            "{} · {} · {}",
            e.project.wallpaper_type.as_label(),
            e.source.label(),
            format_size(e.size_bytes)
        ))
        .size(11)
        .color(pal::accent())
        .font(Font::MONOSPACE),
        text(format!("id {}", e.id))
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        Space::new().height(6),
        text(desc).size(11).color(pal::dim()),
        Space::new().height(8),
        label_inline("RATING", &rating_owned),
        label_inline("TAGS", &tags_owned),
        label_inline("FILE", &e.project.file),
        label_inline("RUNTIME", backend_hint),
        Space::new().height(10),
        settings_block(app),
        Space::new().height(10),
        apply_col,
        Space::new().height(6),
        edit_btn,
        local_mgmt,
        Space::new().height(6),
        unsub,
        Space::new().height(6),
        text(if is_workshop {
            "Enter play · E edit · U unsub"
        } else {
            "Enter play · E edit"
        })
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(16),
    ]
    .width(Fill)
    .padding(Padding {
        top: 0.0,
        right: 12.0,
        bottom: 0.0,
        left: 12.0,
    });

    column![preview, scrollable(scroll_body).height(Fill).width(Fill),]
        .width(Fill)
        .height(Fill)
        .padding(Padding {
            top: 10.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        })
        .into()
}

fn play_monitor_menu(app: &App) -> Element<'_, Message> {
    let mut col = column![text("Play on…")
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE)]
    .spacing(2)
    .padding(6)
    .width(Fill);

    col = col.push(play_mon_option(
        "All displays".into(),
        app.monitor.is_empty(),
        Message::ApplyOnMonitor(String::new()),
    ));
    for m in &app.monitors {
        let on = app.monitor == m.name;
        col = col.push(play_mon_option(
            format!("{}  {}×{}", m.name, m.width, m.height),
            on,
            Message::ApplyOnMonitor(m.name.clone()),
        ));
    }

    container(col)
        .width(Fill)
        .style(|_| container::Style {
            background: Some(Background::Color(pal::panel2())),
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..Default::default()
        })
        .into()
}

fn play_mon_option(label: String, on: bool, msg: Message) -> Element<'static, Message> {
    button(
        container(
            text(label)
                .size(11)
                .color(if on { pal::accent() } else { pal::dim() })
                .font(Font::MONOSPACE),
        )
        .width(Fill)
        .padding(Padding::from([6, 8])),
    )
    .on_press(msg)
    .padding(0)
    .width(Fill)
    .style(move |_t, status| {
        let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if on {
                pal::accent_bg()
            } else if hover {
                pal::panel()
            } else {
                Color::TRANSPARENT
            })),
            text_color: pal::fg(),
            border: Border::default(),
            ..Default::default()
        }
    })
    .into()
}

/// Settings block: full height of its content (not nested-scrollable).
/// Parent detail panel scrolls as a whole.
fn settings_block(app: &App) -> Element<'_, Message> {
    let n = app
        .props
        .iter()
        .filter(|p| !matches!(p.kind, PropKind::Group))
        .count();
    let header = text(format!("SETTINGS · {n} props"))
        .size(11)
        .color(pal::accent())
        .font(Font::MONOSPACE);

    let mut col = column![].spacing(10).width(Fill);
    col = col.push(section_title("PLAYBACK"));
    col = col.push(playback_section(app));
    col = col.push(section_title("LAYOUT"));
    col = col.push(layout_section(app));
    col = col.push(section_title("PROPERTIES"));
    if app.props.is_empty() {
        col = col.push(
            column![
                text("No adjustable properties in project.json")
                    .size(11)
                    .color(pal::mute()),
                text("(only some wallpapers ship user settings)")
                    .size(10)
                    .color(pal::mute()),
            ]
            .spacing(2),
        );
    } else {
        for p in &app.props {
            col = col.push(prop_row(app, p));
        }
        col = col.push(
            button(
                container(
                    text("RESET PROPS TO DEFAULTS")
                        .size(11)
                        .font(Font::MONOSPACE)
                        .color(pal::dim()),
                )
                .width(Fill)
                .center_x(Fill)
                .padding(8),
            )
            .on_press(Message::PropReset)
            .padding(0)
            .width(Fill)
            .style(stop_style),
        );
    }
    col = col.push(
        button(
            container(
                text("RESET LAYOUT / PLAYBACK")
                    .size(11)
                    .font(Font::MONOSPACE)
                    .color(pal::dim()),
            )
            .width(Fill)
            .center_x(Fill)
            .padding(8),
        )
        .on_press(Message::PresentReset)
        .padding(0)
        .width(Fill)
        .style(stop_style),
    );

    container(
        column![header, Space::new().height(8), col]
            .width(Fill)
            .spacing(0),
    )
    .width(Fill)
    .padding(10)
    .style(|_| container::Style {
        background: Some(Background::Color(pal::panel2())),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    })
    .into()
}

pub(crate) fn section_title(s: &str) -> Element<'_, Message> {
    container(text(s).size(10).color(pal::mute()).font(Font::MONOSPACE))
        .padding(Padding {
            top: 4.0,
            right: 0.0,
            bottom: 0.0,
            left: 0.0,
        })
        .into()
}

fn playback_section(app: &App) -> Element<'_, Message> {
    let p = &app.present;
    column![
        row![
            toggle_chip("Mute", p.mute, Message::PresentMute(!p.mute)),
            Space::new().width(6),
            toggle_chip("Pause", p.paused, Message::PresentPause(!p.paused)),
        ]
        .width(Fill),
        Space::new().height(8),
        row![
            text("Timescale")
                .size(12)
                .color(pal::fg())
                .width(Length::Fixed(88.0)),
            text(format!("{:.2}×", p.rate))
                .size(11)
                .color(pal::accent())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(48.0)),
            slider(0.05..=4.0, p.rate, Message::PresentRate)
                .style(slider_style)
                .step(0.05_f32)
                .width(Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    ]
    .spacing(4)
    .width(Fill)
    .into()
}

fn layout_section(app: &App) -> Element<'_, Message> {
    let p = &app.present;
    let fit_selected = FitModeUi::ALL.iter().copied().find(|f| *f == p.fit);
    column![
        row![
            text("Fit")
                .size(12)
                .color(pal::fg())
                .width(Length::Fixed(88.0)),
            pick_list(FitModeUi::ALL, fit_selected, Message::PresentFit)
                .text_size(12)
                .style(pick_style)
                .menu_style(menu_style)
                .padding(6)
                .width(Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        Space::new().height(6),
        row![
            text("Scale")
                .size(12)
                .color(pal::fg())
                .width(Length::Fixed(88.0)),
            text(format!("{:.2}×", p.zoom))
                .size(11)
                .color(pal::accent())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(48.0)),
            slider(0.25..=4.0, p.zoom, Message::PresentZoom)
                .style(slider_style)
                .step(0.01_f32)
                .width(Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        Space::new().height(4),
        row![
            text("Pos X")
                .size(12)
                .color(pal::fg())
                .width(Length::Fixed(88.0)),
            text(format!("{:.2}", p.pos_x))
                .size(11)
                .color(pal::accent())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(48.0)),
            slider(-1.0..=1.0, p.pos_x, Message::PresentPosX)
                .style(slider_style)
                .step(0.01_f32)
                .width(Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        row![
            text("Pos Y")
                .size(12)
                .color(pal::fg())
                .width(Length::Fixed(88.0)),
            text(format!("{:.2}", p.pos_y))
                .size(11)
                .color(pal::accent())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(48.0)),
            slider(-1.0..=1.0, p.pos_y, Message::PresentPosY)
                .style(slider_style)
                .step(0.01_f32)
                .width(Fill),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        Space::new().height(6),
        row![
            toggle_chip("Flip H", p.flip_h, Message::PresentFlipH(!p.flip_h)),
            Space::new().width(6),
            toggle_chip("Flip V", p.flip_v, Message::PresentFlipV(!p.flip_v)),
        ],
        text("Scale / position / flip apply to video wallpapers; pause & rate apply to all.")
            .size(10)
            .color(pal::mute()),
    ]
    .spacing(4)
    .width(Fill)
    .into()
}

pub(crate) fn toggle_chip<'a>(label: &'a str, on: bool, msg: Message) -> Element<'a, Message> {
    button(
        container(
            text(format!("{}  {}", if on { "●" } else { "○" }, label))
                .size(11)
                .font(Font::MONOSPACE)
                .color(if on { pal::accent() } else { pal::dim() }),
        )
        .padding(Padding::from([6, 10])),
    )
    .on_press(msg)
    .padding(0)
    .style(if on { chip_on_style } else { chip_off_style })
    .into()
}

fn prop_row<'a>(app: &'a App, p: &'a wallengine_we::PropDef) -> Element<'a, Message> {
    let key = p.key.clone();
    match (&p.kind, &p.value) {
        (PropKind::Group, _) => container(
            text(p.label.to_uppercase())
                .size(10)
                .color(pal::accent())
                .font(Font::MONOSPACE),
        )
        .padding(Padding {
            top: 8.0,
            right: 0.0,
            bottom: 2.0,
            left: 0.0,
        })
        .into(),
        (PropKind::Bool, PropValue::Bool(v)) => row![
            text(&p.label).size(12).color(pal::fg()).width(Fill),
            checkbox(*v)
                .label(if *v { "On" } else { "Off" })
                .on_toggle({
                    let key = key.clone();
                    move |b| Message::PropBool(key.clone(), b)
                })
                .size(14)
                .text_size(12)
                .style(checkbox_style),
        ]
        .align_y(Alignment::Center)
        .spacing(8)
        .width(Fill)
        .into(),
        (
            PropKind::Slider {
                min,
                max,
                step,
                fraction,
            },
            PropValue::Number(n),
        ) => {
            let n = (*n).clamp(*min, *max);
            let min = *min;
            let max = *max;
            let step = (*step).max(0.001);
            let disp = if *fraction {
                format!("{n:.2}")
            } else {
                format!("{n:.0}")
            };
            column![
                row![
                    text(&p.label).size(12).color(pal::fg()).width(Fill),
                    text(disp)
                        .size(11)
                        .color(pal::accent())
                        .font(Font::MONOSPACE),
                ],
                slider(min..=max, n, {
                    let key = key.clone();
                    move |v| Message::PropSlider(key.clone(), v)
                })
                .style(slider_style)
                .step(step)
                .width(Fill),
                text(format!("{min} – {max}"))
                    .size(10)
                    .color(pal::mute())
                    .font(Font::MONOSPACE),
            ]
            .spacing(4)
            .width(Fill)
            .into()
        }
        (PropKind::Color, PropValue::Color([r, g, b])) => {
            color_picker_row(app, &p.label, p.key.as_str(), *r, *g, *b)
        }
        (PropKind::Combo { options }, PropValue::Text(cur)) => {
            let labels: Vec<String> = options.iter().map(|(l, _)| l.clone()).collect();
            let selected = options
                .iter()
                .find(|(_, v)| v == cur)
                .map(|(l, _)| l.clone())
                .or_else(|| labels.first().cloned());
            let opts = options.clone();
            column![
                text(&p.label).size(12).color(pal::fg()),
                pick_list(labels, selected, move |lab| {
                    let val = opts
                        .iter()
                        .find(|(l, _)| l == &lab)
                        .map(|(_, v)| v.clone())
                        .unwrap_or(lab);
                    Message::PropCombo(key.clone(), val)
                })
                .text_size(12)
                .style(pick_style)
                .menu_style(menu_style)
                .padding(6)
                .width(Fill),
            ]
            .spacing(4)
            .width(Fill)
            .into()
        }
        (PropKind::Text, PropValue::Text(s)) | (PropKind::Other(_), PropValue::Text(s)) => {
            // Pure info text (empty value + long label) → read-only.
            if s.is_empty() && p.label.len() > 40 {
                text(&p.label).size(11).color(pal::mute()).into()
            } else {
                let key2 = key.clone();
                column![
                    text(&p.label).size(12).color(pal::fg()),
                    text_input("", s)
                        .on_input(move |v| Message::PropText(key2.clone(), v))
                        .size(12)
                        .padding(6),
                ]
                .spacing(4)
                .width(Fill)
                .into()
            }
        }
        _ => text(format!("(unsupported: {})", p.key))
            .size(11)
            .color(pal::mute())
            .into(),
    }
}

fn color_picker_row<'a>(
    app: &'a App,
    label: &'a str,
    key: &'a str,
    r: f32,
    g: f32,
    b: f32,
) -> Element<'a, Message> {
    let open = app.color_open.as_deref() == Some(key);
    let swatch = container(Space::new().width(Fill).height(Fill))
        .width(Length::Fixed(36.0))
        .height(Length::Fixed(22.0))
        .style(move |_| container::Style {
            background: Some(Background::Color(Color::from_rgb(r, g, b))),
            border: Border {
                color: pal::line(),
                width: 1.0,
                radius: 3.0.into(),
            },
            ..Default::default()
        });

    let hex = if open && !app.color_hex_draft.is_empty() {
        app.color_hex_draft.clone()
    } else {
        format!(
            "#{:02X}{:02X}{:02X}",
            (r.clamp(0.0, 1.0) * 255.0).round() as u8,
            (g.clamp(0.0, 1.0) * 255.0).round() as u8,
            (b.clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    };

    let header = row![
        text(label).size(12).color(pal::fg()).width(Fill),
        swatch,
        Space::new().width(8),
        button(
            container(
                text(if open { "▲" } else { "▼" })
                    .size(11)
                    .color(pal::dim())
                    .font(Font::MONOSPACE),
            )
            .padding(Padding::from([4, 8])),
        )
        .on_press(Message::ToggleColorOpen(key.to_string()))
        .padding(0)
        .style(chip_off_style),
    ]
    .spacing(6)
    .align_y(Alignment::Center)
    .width(Fill);

    if !open {
        return column![header].spacing(4).width(Fill).into();
    }

    let key_r = key.to_string();
    let key_g = key.to_string();
    let key_b = key.to_string();
    let key_hex = key.to_string();

    column![
        header,
        row![
            text("R")
                .size(11)
                .color(Color::from_rgb(0.9, 0.35, 0.35))
                .width(Length::Fixed(14.0)),
            slider(0.0..=1.0, r, {
                let key = key_r;
                move |v| Message::PropColor(key.clone(), v, g, b)
            })
            .style(slider_style)
            .step(0.001_f32)
            .width(Fill),
            text(format!("{:.0}", r * 255.0))
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(28.0)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        row![
            text("G")
                .size(11)
                .color(Color::from_rgb(0.4, 0.85, 0.4))
                .width(Length::Fixed(14.0)),
            slider(0.0..=1.0, g, {
                let key = key_g;
                move |v| Message::PropColor(key.clone(), r, v, b)
            })
            .style(slider_style)
            .step(0.001_f32)
            .width(Fill),
            text(format!("{:.0}", g * 255.0))
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(28.0)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        row![
            text("B")
                .size(11)
                .color(Color::from_rgb(0.4, 0.55, 0.95))
                .width(Length::Fixed(14.0)),
            slider(0.0..=1.0, b, {
                let key = key_b;
                move |v| Message::PropColor(key.clone(), r, g, v)
            })
            .style(slider_style)
            .step(0.001_f32)
            .width(Fill),
            text(format!("{:.0}", b * 255.0))
                .size(10)
                .color(pal::mute())
                .font(Font::MONOSPACE)
                .width(Length::Fixed(28.0)),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
        row![
            text("Hex")
                .size(11)
                .color(pal::dim())
                .width(Length::Fixed(32.0)),
            text_input("#RRGGBB", &hex)
                .on_input(move |v| Message::PropColorHex(key_hex.clone(), v))
                .size(12)
                .padding(6)
                .width(Fill),
            container(Space::new().width(Fill).height(Fill))
                .width(Length::Fixed(28.0))
                .height(Length::Fixed(28.0))
                .style(move |_| container::Style {
                    background: Some(Background::Color(Color::from_rgb(r, g, b))),
                    border: Border {
                        color: pal::accent(),
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                }),
        ]
        .spacing(6)
        .align_y(Alignment::Center),
    ]
    .spacing(6)
    .width(Fill)
    .into()
}

pub(crate) fn chip_on_style(theme: &iced::Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let base = button::Style {
        background: Some(Background::Color(crate::theme::mix(
            pal::accent_bg(),
            pal::accent(),
            0.10,
        ))),
        text_color: pal::accent(),
        border: Border {
            color: pal::accent(),
            width: 1.0,
            radius: (pal::radius() + 1.0).into(),
        },
        ..Default::default()
    };
    match status {
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(crate::theme::mix(
                pal::accent_bg(),
                pal::accent(),
                0.18,
            ))),
            ..base
        },
        _ => base,
    }
}

pub(crate) fn chip_off_style(theme: &iced::Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let base = button::Style {
        background: Some(Background::Color(pal::panel())),
        text_color: pal::dim(),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: (pal::radius() + 1.0).into(),
        },
        ..Default::default()
    };
    match status {
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(pal::panel2())),
            ..base
        },
        _ => base,
    }
}

// ── atoms ──────────────────────────────────────────────────────────────────

fn label(s: &str) -> Element<'_, Message> {
    container(text(s).size(10).color(pal::mute()).font(Font::MONOSPACE))
        .padding(Padding {
            top: 12.0,
            right: 12.0,
            bottom: 6.0,
            left: 12.0,
        })
        .into()
}

pub(crate) fn label_inline(k: &str, v: &str) -> Element<'static, Message> {
    column![
        text(k.to_string())
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        text(v.to_string())
            .size(11)
            .color(pal::dim())
            .font(Font::MONOSPACE),
        Space::new().height(6),
    ]
    .into()
}

fn truncate_chars(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

pub(crate) fn filter_btn(label: &str, on: bool, msg: Message) -> Element<'_, Message> {
    let mark = if on { "▣" } else { "□" };
    button(
        row![
            text(mark)
                .size(13)
                .color(if on { pal::accent() } else { pal::mute() })
                .font(Font::MONOSPACE),
            Space::new().width(8),
            text(label)
                .size(13)
                .color(if on { pal::fg() } else { pal::dim() }),
        ]
        .align_y(Alignment::Center)
        .padding(Padding::from([7, 12])),
    )
    .on_press(msg)
    .padding(0)
    .width(Fill)
    .style(move |_t, status| {
        let bg = if matches!(status, button::Status::Hovered) {
            pal::panel2()
        } else if on {
            pal::accent_bg()
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
            text_color: pal::fg(),
            shadow: Default::default(),
            snap: true,
        }
    })
    .into()
}

pub(crate) fn flat(label: impl Into<String>, msg: Message) -> Element<'static, Message> {
    let label = label.into();
    button(text(label).size(11).font(Font::MONOSPACE))
        .on_press(msg)
        .padding(Padding::from([6, 10]))
        .style(|_t, status| {
            let (bg, border) = match status {
                button::Status::Hovered => {
                    (pal::panel2(), crate::theme::lighten(pal::line(), 0.15))
                }
                _ => (Color::TRANSPARENT, pal::line()),
            };
            button::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: border,
                    width: 1.0,
                    radius: 0.0.into(),
                },
                text_color: pal::fg(),
                shadow: Default::default(),
                snap: true,
            }
        })
        .into()
}

pub(crate) fn panel(bg: Color) -> container::Style {
    container::Style {
        background: Some(pal::panel_fill(bg)),
        border: Border {
            radius: 0.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub(crate) fn hrule() -> Element<'static, Message> {
    rule::horizontal(1)
        .style(|_| rule::Style {
            color: pal::line(),
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

pub(crate) fn vrule() -> Element<'static, Message> {
    rule::vertical(1)
        .style(|_| rule::Style {
            color: pal::line(),
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
        .into()
}

/// Dropdown styled like the rest of the chrome — iced's default light pick
/// list is jarring against a dark, wallpaper-tinted panel.
pub(crate) fn pick_style(
    _t: &iced::Theme,
    status: iced::widget::pick_list::Status,
) -> iced::widget::pick_list::Style {
    use iced::widget::pick_list::Status;
    let (bg, border) = match status {
        Status::Hovered | Status::Opened { .. } => (pal::panel2(), pal::accent()),
        Status::Active => (pal::panel(), pal::line()),
    };
    iced::widget::pick_list::Style {
        text_color: pal::fg(),
        placeholder_color: pal::mute(),
        handle_color: pal::accent(),
        background: Background::Color(bg),
        border: Border {
            color: border,
            width: 1.0,
            radius: pal::radius().into(),
        },
    }
}

/// Open-dropdown list, matching [`pick_style`].
pub(crate) fn menu_style(_t: &iced::Theme) -> iced::widget::overlay::menu::Style {
    iced::widget::overlay::menu::Style {
        background: Background::Color(pal::panel2()),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: pal::radius().into(),
        },
        text_color: pal::fg(),
        selected_text_color: pal::on_accent(),
        selected_background: pal::accent_fill(),
        shadow: Default::default(),
    }
}

/// Accent-coloured slider.
pub(crate) fn slider_style(
    _t: &iced::Theme,
    status: iced::widget::slider::Status,
) -> iced::widget::slider::Style {
    use iced::widget::slider::{Handle, HandleShape, Rail, Status};
    let active = matches!(status, Status::Hovered | Status::Dragged);
    let fill = if active {
        pal::accent_hi()
    } else {
        pal::accent()
    };
    iced::widget::slider::Style {
        rail: Rail {
            backgrounds: (
                Background::Color(fill),
                Background::Color(crate::theme::mix(pal::line(), pal::panel(), 0.4)),
            ),
            width: 4.0,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: 2.0.into(),
            },
        },
        handle: Handle {
            shape: HandleShape::Circle { radius: 7.0 },
            background: Background::Color(fill),
            border_width: 1.0,
            border_color: crate::theme::lighten(fill, 0.25),
        },
    }
}

/// Checkbox in the wallpaper's colour.
pub(crate) fn checkbox_style(
    _t: &iced::Theme,
    status: iced::widget::checkbox::Status,
) -> iced::widget::checkbox::Style {
    use iced::widget::checkbox::Status;
    let (checked, hovered) = match status {
        Status::Active { is_checked } => (is_checked, false),
        Status::Hovered { is_checked } => (is_checked, true),
        Status::Disabled { is_checked } => (is_checked, false),
    };
    let bg = if checked {
        pal::accent()
    } else if hovered {
        pal::panel2()
    } else {
        pal::panel()
    };
    iced::widget::checkbox::Style {
        background: Background::Color(bg),
        icon_color: pal::on_accent(),
        border: Border {
            color: if checked { pal::accent() } else { pal::line() },
            width: 1.0,
            radius: (pal::radius() * 0.7).into(),
        },
        text_color: Some(pal::fg()),
    }
}

pub(crate) fn search_style(_t: &iced::Theme, status: text_input::Status) -> text_input::Style {
    let border = match status {
        text_input::Status::Focused { .. } => pal::accent(),
        _ => pal::line(),
    };
    text_input::Style {
        background: Background::Color(pal::panel2()),
        border: Border {
            color: border,
            width: 1.0,
            radius: 0.0.into(),
        },
        icon: pal::dim(),
        placeholder: pal::mute(),
        value: pal::fg(),
        selection: pal::accent(),
    }
}

pub(crate) fn apply_style(_t: &iced::Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => pal::accent_fill_hover(),
        _ => pal::accent_fill(),
    };
    button::Style {
        background: Some(bg),
        border: Border {
            radius: pal::radius().into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        text_color: pal::on_accent(),
        shadow: Default::default(),
        snap: true,
    }
}

fn edit_style(_t: &iced::Theme, status: button::Status) -> button::Style {
    let bg = match status {
        button::Status::Hovered | button::Status::Pressed => Color::from_rgb(0.45, 0.62, 0.48),
        _ => Color::from_rgb(0.35, 0.52, 0.40),
    };
    button::Style {
        background: Some(Background::Color(bg)),
        border: Border {
            radius: 0.0.into(),
            width: 0.0,
            color: bg,
        },
        text_color: pal::bg(),
        shadow: Default::default(),
        snap: true,
    }
}

/// Destructive action button. Once armed it turns solid red and grows a
/// Cancel next to it, so a second stray press can't be an accident.
fn danger_btn<'a>(label: &'a str, armed: bool, msg: Message) -> Element<'a, Message> {
    if !armed {
        return button(
            container(text(label).size(11).font(Font::MONOSPACE).color(pal::err()))
                .width(Fill)
                .center_x(Fill)
                .padding(8),
        )
        .on_press(msg)
        .padding(0)
        .width(Fill)
        .style(stop_style)
        .into();
    }
    row![
        button(
            container(
                text(format!("CONFIRM · {label}"))
                    .size(11)
                    .font(Font::MONOSPACE)
                    .color(crate::theme::on_color(pal::err())),
            )
            .width(Fill)
            .center_x(Fill)
            .padding(8),
        )
        .on_press(msg)
        .padding(0)
        .width(Fill)
        .style(|_t: &iced::Theme, status: button::Status| {
            let bg = match status {
                button::Status::Hovered | button::Status::Pressed => {
                    crate::theme::lighten(pal::err(), 0.10)
                }
                _ => pal::err(),
            };
            button::Style {
                background: Some(Background::Color(bg)),
                border: Border {
                    color: bg,
                    width: 1.0,
                    radius: pal::radius().into(),
                },
                text_color: crate::theme::on_color(pal::err()),
                shadow: Default::default(),
                snap: true,
            }
        }),
        Space::new().width(6),
        flat("Cancel", Message::CancelConfirm),
    ]
    .align_y(Alignment::Center)
    .width(Fill)
    .into()
}

pub(crate) fn stop_style(_t: &iced::Theme, status: button::Status) -> button::Style {
    let border = match status {
        button::Status::Hovered => pal::err(),
        _ => pal::line(),
    };
    button::Style {
        background: Some(Background::Color(Color::TRANSPARENT)),
        border: Border {
            color: border,
            width: 1.0,
            radius: 0.0.into(),
        },
        text_color: pal::fg(),
        shadow: Default::default(),
        snap: true,
    }
}
