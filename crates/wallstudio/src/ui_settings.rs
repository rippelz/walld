//! The Settings tab.
//!
//! Two columns: the controls on the left, a live theme preview on the right so
//! colour choices can be judged without leaving the page. Every control writes
//! straight through to `settings.json` — there is no Apply button and nothing
//! to lose by closing the window.

use crate::settings::{AccentFollow, AccentSource, PreviewAnim, VideoCap};
use crate::theme::{self, pal};
use crate::ui::{
    apply_style, filter_btn, flat, hrule, panel, search_style, stop_style, toggle_chip, vrule,
};
use crate::{App, FitModeUi, MainTab, Message, SettingsMessage as S};
use iced::widget::{
    column, container, pick_list, row, scrollable, slider, text, text_input, Space,
};
use iced::{Alignment, Background, Border, Element, Fill, Font, Length, Padding};
use wallengine_we::PlayBackend;

pub fn view(app: &App) -> Element<'_, Message> {
    // The preview column is a luxury on a narrow window; drop it first.
    let show_preview = app.window_width >= 980.0;
    let body = column![
        appearance_section(app),
        Space::new().height(18),
        quality_section(app),
        Space::new().height(18),
        playback_section(app),
        Space::new().height(18),
        behaviour_section(app),
        Space::new().height(18),
        about_section(app),
        Space::new().height(28),
    ]
    .width(Fill)
    .padding(Padding::from([18, 22]));

    let left = container(scrollable(body).height(Fill).width(Fill))
        .width(Fill)
        .height(Fill)
        .style(|_| panel(pal::bg()));

    if !show_preview {
        return left.into();
    }
    row![
        left,
        vrule(),
        container(preview_column(app))
            .width(Length::Fixed(300.0))
            .height(Fill)
            .style(|_| panel(pal::panel())),
    ]
    .height(Fill)
    .into()
}

// ── sections ───────────────────────────────────────────────────────────────

fn appearance_section(app: &App) -> Element<'_, Message> {
    let a = &app.settings.appearance;
    let dynamic = a.dynamic_accent;

    let mut col = column![
        row_toggle(
            "Color from wallpaper",
            "Take the UI color from the wallpaper you're looking at instead of one fixed accent.",
            dynamic,
            S::DynamicAccent(!dynamic),
        ),
        Space::new().height(6),
    ]
    .width(Fill);

    if dynamic {
        col = col.push(pick_row(
            "Color source",
            AccentSource::ALL.to_vec(),
            Some(a.source),
            |v| Message::Settings(S::AccentSource(v)),
        ));
        col = col.push(hint(match a.source {
            AccentSource::Auto => {
                "Wallpaper Engine's scheme color when the author set one, otherwise the \
                 dominant color of the preview image."
            }
            AccentSource::Scheme => {
                "Only the author's schemecolor. Wallpapers without one fall back to the \
                 fixed accent below."
            }
            AccentSource::Preview => {
                "Always sampled from the preview image, ignoring the author's schemecolor."
            }
        }));
        col = col.push(Space::new().height(6));
        col = col.push(pick_row(
            "Follow",
            AccentFollow::ALL.to_vec(),
            Some(a.follow),
            |v| Message::Settings(S::AccentFollow(v)),
        ));
        col = col.push(hint(match a.follow {
            AccentFollow::Selected => "The theme previews as you move through the gallery.",
            AccentFollow::Playing => {
                "The theme matches what's actually on the desktop, however you browse."
            }
        }));
    } else {
        col = col.push(hint(
            "Dynamic color is off — the accent below is used everywhere, whatever is playing.",
        ));
    }

    col = col.push(Space::new().height(10));
    col = col.push(hex_row(app));
    col = col.push(Space::new().height(10));
    col = col.push(slider_row(
        "Tint strength",
        format!("{:.0}%", a.tint * 100.0),
        0.0..=1.0,
        a.tint,
        0.01,
        |v| Message::Settings(S::Tint(v)),
    ));
    col = col.push(hint(
        "How far panels and borders drift toward the accent's hue. 0% keeps the classic \
         neutral greys and only colors the highlights.",
    ));
    col = col.push(Space::new().height(6));
    col = col.push(slider_row(
        "Corner radius",
        format!("{:.0}px", a.radius),
        0.0..=16.0,
        a.radius,
        1.0,
        |v| Message::Settings(S::Radius(v)),
    ));
    col = col.push(Space::new().height(8));
    col = col.push(
        row![
            toggle_chip(
                "Gradients",
                a.gradients,
                Message::Settings(S::Gradients(!a.gradients)),
            ),
            Space::new().width(8),
            flat(
                "Re-read wallpaper colors",
                Message::Settings(S::RecomputeAccents)
            ),
            Space::new().width(8),
            flat("Reset appearance", Message::Settings(S::ResetAppearance)),
        ]
        .align_y(Alignment::Center),
    );

    section("APPEARANCE", "How wallstudio colors itself", col.into())
}

fn quality_section(app: &App) -> Element<'_, Message> {
    let q = &app.settings.quality;
    let col = column![
        slider_row(
            "Wallpaper FPS",
            format!("{} fps", q.scene_fps),
            5.0..=120.0,
            q.scene_fps as f32,
            1.0,
            |v| Message::Settings(S::SceneFps(v.round() as u32)),
        ),
        hint(
            "walld's frame cap for scenes and video wallpapers. Lower is cooler and quieter; \
             60 matches most authored content. Applied live.",
        ),
        Space::new().height(8),
        pick_row(
            "Video resolution",
            VideoCap::ALL.to_vec(),
            Some(q.video_cap),
            |v| { Message::Settings(S::VideoCap(v)) }
        ),
        hint(
            "Decode cap for video wallpapers. Native matches your largest display — cap it \
             lower to save VRAM and decode time on 4K sources.",
        ),
        Space::new().height(8),
        slider_row(
            "LWE frame cap",
            format!("{} fps", q.lwe_fps),
            5.0..=144.0,
            q.lwe_fps as f32,
            1.0,
            |v| Message::Settings(S::LweFps(v.round() as u32)),
        ),
        hint("Only used when the engine is linux-wallpaperengine. Applies on the next Play."),
        Space::new().height(12),
        pick_row(
            "Animate previews",
            PreviewAnim::ALL.to_vec(),
            Some(q.preview_anim),
            |v| Message::Settings(S::PreviewAnim(v)),
        ),
        hint(
            "Animated GIF previews are the library's main cost. Turning them down (or off) \
             stops wallstudio redrawing constantly while you browse.",
        ),
        Space::new().height(6),
        slider_row(
            "Preview rate",
            format!("{} fps", q.preview_fps),
            1.0..=60.0,
            q.preview_fps as f32,
            1.0,
            |v| Message::Settings(S::PreviewFps(v.round() as u32)),
        ),
        Space::new().height(6),
        slider_row(
            "Tile size",
            format!("{}px", q.tile_size),
            140.0..=400.0,
            q.tile_size as f32,
            5.0,
            |v| Message::Settings(S::TileSize(v.round() as u32)),
        ),
        hint("Target width of a gallery tile; columns still fill the window exactly."),
        Space::new().height(8),
        row![flat("Reset quality", Message::Settings(S::ResetQuality))],
    ]
    .width(Fill);

    section(
        "QUALITY",
        "Render cost on the desktop and in this window",
        col.into(),
    )
}

fn playback_section(app: &App) -> Element<'_, Message> {
    let engine_label = match app.engine {
        PlayBackend::Lwe => "linux-wallpaperengine",
        _ => "walld (built in)",
    };
    let engine_ok = match app.engine {
        PlayBackend::Lwe => app.runtime.lwe_ready,
        _ => app.runtime.engine_ready,
    };
    let monitor = if app.monitor.is_empty() {
        "All displays".to_string()
    } else {
        app.monitor.clone()
    };

    let col = column![
        row![
            key_label("Engine"),
            text(engine_label)
                .size(12)
                .color(if engine_ok { pal::ok() } else { pal::err() })
                .font(Font::MONOSPACE)
                .width(Fill),
            flat("Switch", Message::ToggleEngine),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        hint(if engine_ok {
            "Ready."
        } else {
            "Not reachable right now — walld starts on demand, LWE must be installed."
        }),
        Space::new().height(8),
        row![
            key_label("Display"),
            text(monitor)
                .size(12)
                .color(pal::fg())
                .font(Font::MONOSPACE)
                .width(Fill),
            flat("Pick in top bar", Message::SetTab(MainTab::Library)),
        ]
        .align_y(Alignment::Center)
        .spacing(8),
        Space::new().height(8),
        pick_row(
            "Default fit",
            FitModeUi::ALL.to_vec(),
            Some(app.settings.behaviour.default_fit),
            |v| Message::Settings(S::DefaultFit(v)),
        ),
        hint("Used for wallpapers you haven't given a layout yet."),
        Space::new().height(8),
        row![
            toggle_chip("Start muted", app.silent, Message::ToggleSilent),
            Space::new().width(8),
            toggle_chip(
                if app.sort_newest {
                    "Sort: newest"
                } else {
                    "Sort: name"
                },
                true,
                Message::ToggleSort,
            ),
        ],
    ]
    .width(Fill);

    section("PLAYBACK", "Defaults for playing a wallpaper", col.into())
}

fn behaviour_section(app: &App) -> Element<'_, Message> {
    let b = &app.settings.behaviour;
    let col =
        column![
        row_toggle(
            "Confirm unsubscribe & delete",
            "Ask twice before removing files. U is one keystroke away from wiping a download.",
            b.confirm_destructive,
            S::ConfirmDestructive(!b.confirm_destructive),
        ),
        Space::new().height(6),
        row_toggle(
            "Match the desktop to wallpaper",
            "Generate a matching palette for waybar, Kitty text, KDE/Qt apps (including Dolphin), \
             GTK, Hyprland borders, mako and rofi whenever you play a wallpaper.",
            b.desktop_accent,
            S::DesktopAccent(!b.desktop_accent),
        ),
        Space::new().height(4),
        row![flat("Match desktop now", Message::Settings(S::RecolorDesktop))],
        Space::new().height(6),
        row_toggle(
            "Smooth transitions",
            "Fade waybar and window borders to the new accent over ~0.8s instead of snapping. \
             The background wipe comes from walld itself.",
            b.smooth_transition,
            S::SmoothTransition(!b.smooth_transition),
        ),
        Space::new().height(6),
        row_toggle(
            "Auto-refresh library",
            "Notice wallpapers Steam finished downloading while wallstudio is open.",
            b.auto_refresh,
            S::AutoRefresh(!b.auto_refresh),
        ),
    ]
        .width(Fill);

    section("BEHAVIOUR", "Safety nets and housekeeping", col.into())
}

fn about_section(app: &App) -> Element<'_, Message> {
    let col = column![
        crate::ui::label_inline(
            "SETTINGS FILE",
            &crate::settings::settings_path().display().to_string()
        ),
        crate::ui::label_inline(
            "SESSION FILE",
            &crate::settings::config_dir()
                .join("session.json")
                .display()
                .to_string()
        ),
        crate::ui::label_inline("WALLD CONFIG", &crate::walld_config_display()),
        crate::ui::label_inline(
            "WORKSHOP",
            &wallengine_we::workshop_dir().display().to_string()
        ),
        crate::ui::label_inline(
            "LIBRARY",
            &format!("{} wallpapers installed", app.entries.len()),
        ),
        Space::new().height(4),
        row![
            flat("Open config folder", Message::Settings(S::OpenConfigDir)),
            Space::new().width(8),
            danger("RESET ALL SETTINGS", Message::Settings(S::ResetAll)),
        ]
        .align_y(Alignment::Center),
    ]
    .width(Fill);

    section("PATHS", "Where wallstudio keeps things", col.into())
}

// ── live preview ───────────────────────────────────────────────────────────

fn preview_column(app: &App) -> Element<'_, Message> {
    let p = pal::get();
    let a = &app.settings.appearance;
    let origin = if !a.dynamic_accent {
        "fixed accent".to_string()
    } else if app.tab == MainTab::Settings {
        match app.accent_subject_label() {
            Some(name) => name,
            None => "no wallpaper selected".into(),
        }
    } else {
        "wallpaper".into()
    };

    let body = column![
        crate::ui::section_title("LIVE PREVIEW"),
        Space::new().height(6),
        swatch_strip(),
        Space::new().height(10),
        text(format!("accent {}", theme::to_hex(p.accent)))
            .size(11)
            .color(pal::accent())
            .font(Font::MONOSPACE),
        text(format!("from {origin}"))
            .size(10)
            .color(pal::mute())
            .font(Font::MONOSPACE),
        Space::new().height(14),
        // A miniature of the real chrome: primary button, chips, a filter row.
        iced::widget::button(
            container(
                text("PLAY")
                    .size(12)
                    .font(Font::MONOSPACE)
                    .color(pal::on_accent()),
            )
            .width(Fill)
            .center_x(Fill)
            .padding(10),
        )
        .on_press(Message::Settings(S::RecomputeAccents))
        .padding(0)
        .width(Fill)
        .style(apply_style),
        Space::new().height(8),
        row![
            toggle_chip("On", true, Message::Settings(S::RecomputeAccents)),
            Space::new().width(6),
            toggle_chip("Off", false, Message::Settings(S::RecomputeAccents)),
        ],
        Space::new().height(8),
        filter_btn("Selected row", true, Message::Settings(S::RecomputeAccents)),
        filter_btn("Plain row", false, Message::Settings(S::RecomputeAccents)),
        Space::new().height(10),
        text("Body text").size(13).color(pal::fg()),
        text("Secondary text").size(12).color(pal::dim()),
        text("Muted caption").size(11).color(pal::mute()),
        Space::new().height(10),
        hrule(),
        Space::new().height(10),
        text(if p.gradients {
            "Gradients on"
        } else {
            "Gradients off"
        })
        .size(10)
        .color(pal::mute())
        .font(Font::MONOSPACE),
        Space::new().height(10),
        flat(
            "Pin this color as fixed",
            Message::Settings(S::PinCurrentAccent)
        ),
    ]
    .width(Fill)
    .padding(Padding::from([16, 14]));

    scrollable(body).height(Fill).width(Fill).into()
}

fn swatch_strip() -> Element<'static, Message> {
    let p = pal::get();
    let cells = [
        ("bg", p.bg),
        ("panel", p.panel),
        ("line", p.line),
        ("accent", p.accent),
        ("hi", p.accent_hi),
        ("lo", p.accent_lo),
    ];
    let mut r = row![].spacing(4);
    for (name, c) in cells {
        r = r.push(
            column![
                container(Space::new().width(Fill).height(Length::Fixed(30.0)))
                    .width(Fill)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(c)),
                        border: Border {
                            color: pal::line(),
                            width: 1.0,
                            radius: pal::radius().into(),
                        },
                        ..Default::default()
                    }),
                text(name).size(8).color(pal::mute()).font(Font::MONOSPACE),
            ]
            .spacing(3)
            .width(Fill),
        );
    }
    r.width(Fill).into()
}

// ── atoms ──────────────────────────────────────────────────────────────────

fn section<'a>(title: &'a str, blurb: &'a str, body: Element<'a, Message>) -> Element<'a, Message> {
    container(
        column![
            text(title)
                .size(12)
                .color(pal::accent())
                .font(Font::MONOSPACE),
            text(blurb).size(10).color(pal::mute()),
            Space::new().height(12),
            body,
        ]
        .width(Fill),
    )
    .width(Fill)
    .padding(14)
    .style(|_| container::Style {
        background: Some(pal::panel_fill(pal::panel())),
        border: Border {
            color: pal::line(),
            width: 1.0,
            radius: pal::radius().into(),
        },
        ..Default::default()
    })
    .into()
}

fn key_label(s: &str) -> Element<'_, Message> {
    text(s)
        .size(12)
        .color(pal::fg())
        .width(Length::Fixed(120.0))
        .into()
}

fn hint(s: &str) -> Element<'_, Message> {
    container(text(s).size(10).color(pal::mute()))
        .padding(Padding {
            top: 2.0,
            right: 0.0,
            bottom: 0.0,
            left: 2.0,
        })
        .width(Fill)
        .into()
}

/// A labelled on/off row with an explanatory line underneath.
fn row_toggle<'a>(title: &'a str, blurb: &'a str, on: bool, msg: S) -> Element<'a, Message> {
    column![row![
        column![
            text(title).size(13).color(pal::fg()),
            text(blurb).size(10).color(pal::mute()),
        ]
        .spacing(2)
        .width(Fill),
        toggle_chip(if on { "On" } else { "Off" }, on, Message::Settings(msg)),
    ]
    .align_y(Alignment::Center)
    .spacing(10)
    .width(Fill),]
    .width(Fill)
    .into()
}

fn slider_row<'a>(
    label: &'a str,
    value_text: String,
    range: std::ops::RangeInclusive<f32>,
    value: f32,
    step: f32,
    on_change: impl Fn(f32) -> Message + 'a,
) -> Element<'a, Message> {
    row![
        key_label(label),
        text(value_text)
            .size(11)
            .color(pal::accent())
            .font(Font::MONOSPACE)
            .width(Length::Fixed(56.0)),
        slider(range, value, on_change)
            .style(crate::ui::slider_style)
            .step(step)
            .width(Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .width(Fill)
    .into()
}

fn pick_row<'a, T>(
    label: &'a str,
    options: Vec<T>,
    selected: Option<T>,
    on_select: impl Fn(T) -> Message + 'a,
) -> Element<'a, Message>
where
    T: ToString + PartialEq + Clone + 'a,
{
    row![
        key_label(label),
        pick_list(options, selected, on_select)
            .text_size(12)
            .style(crate::ui::pick_style)
            .menu_style(crate::ui::menu_style)
            .padding(6)
            .width(Fill),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .width(Fill)
    .into()
}

/// Fixed-accent hex field with a swatch, plus a "use what's on screen" button.
fn hex_row(app: &App) -> Element<'_, Message> {
    let c = app.settings.appearance.fixed_accent;
    row![
        key_label("Fixed accent"),
        text_input("#RRGGBB", &app.accent_hex_draft)
            .on_input(|v| Message::Settings(S::FixedAccentHex(v)))
            .size(12)
            .padding(6)
            .width(Length::Fixed(110.0))
            .style(search_style),
        container(Space::new().width(Fill).height(Fill))
            .width(Length::Fixed(28.0))
            .height(Length::Fixed(28.0))
            .style(move |_| container::Style {
                background: Some(Background::Color(c)),
                border: Border {
                    color: pal::line(),
                    width: 1.0,
                    radius: pal::radius().into(),
                },
                ..Default::default()
            }),
        Space::new().width(Fill),
        flat("Use current", Message::Settings(S::PinCurrentAccent)),
    ]
    .spacing(8)
    .align_y(Alignment::Center)
    .width(Fill)
    .into()
}

fn danger(label: &str, msg: Message) -> Element<'_, Message> {
    iced::widget::button(
        container(text(label).size(11).font(Font::MONOSPACE).color(pal::err()))
            .padding(Padding::from([6, 10])),
    )
    .on_press(msg)
    .padding(0)
    .style(stop_style)
    .into()
}
