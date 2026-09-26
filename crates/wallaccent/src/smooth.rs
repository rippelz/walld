//! The smooth recolor.
//!
//! Neither waybar nor Hyprland grew a "fade the colours" animation of their
//! own, so the fade leans on what each *can* animate natively:
//!
//! - **waybar**: `accent.css` carries a CSS `transition` rule, so a single
//!   SIGUSR2 hot-swap re-reads the stylesheet and GTK tweens every recolored
//!   element per frame for `FADE_MS`. A step engine that rewrote the CSS nine
//!   times instead lost half its steps to waybar's built-in reload debounce
//!   (~90 ms) and looked choppier than no fade at all — that was the bug.
//! - **Hyprland borders**: pushed live through hyprctl in small steps; hyprctl
//!   has no debounce, so these land every frame.
//!
//! kitty/mako/rofi/fastfetch land with the final write: kitty on the next
//! window, mako/rofi on the next popup, fastfetch next run. Nothing to step.

use crate::ramp::Ramp;
use std::time::{Duration, Instant};

/// How long the bar + border tween runs.
pub const FADE_MS: u64 = 800;
/// Border steps across the fade (hyprctl is cheap + not debounced).
const FRAMES: usize = 12;

/// The rule that makes a plain CSS hot-swap animate. `window#waybar, *`
/// covers both bars (the winmode taskbar runs under its own window name).
pub const CSS_TRANSITION: &str =
    "window#waybar, window#waybar * { transition: background-color 800ms linear, \
     color 800ms linear, border-color 800ms linear; }";

/// Whether a fade from `a` to `b` is worth doing at all.
pub fn step_count(a: &Ramp, b: &Ramp) -> usize {
    let d = (0..3)
        .map(|i| a.accent[i] - b.accent[i])
        .map(|x| x * x)
        .sum::<f32>()
        .sqrt();
    if d < 0.03 {
        0
    } else {
        1
    }
}

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t) * (1.0 - t)
}

/// Walk the Hyprland borders from `from` to `to` over `FADE_MS`, then hold
/// until the waybar tween (running on GTK's own clock) has finished.
///
/// Runs on the caller's thread: the process lives only to recolor, so it must
/// stay up until the last border frame lands (a detached thread would die with
/// the process before its first step).
pub fn run(from: Ramp, to: Ramp) {
    if step_count(&from, &to) == 0 {
        return;
    }
    let start = Instant::now();
    for k in 1..=FRAMES {
        let t = ease_out(k as f32 / FRAMES as f32);
        let mid = from.lerp(&to, t);
        // Pace against the clock, not cumulative work, so a slow hyprctl call
        // doesn't stretch the fade.
        let want = start + Duration::from_millis(FADE_MS * k as u64 / FRAMES as u64);
        let now = Instant::now();
        if now < want {
            std::thread::sleep(want - now);
        }
        set_borders(&mid);
    }
    // The bar tween started with the single SIGUSR2; let it finish before we
    // exit so the last CSS hot-swap isn't dropped.
    let elapsed = start.elapsed();
    let total = Duration::from_millis(FADE_MS + 150);
    if elapsed < total {
        std::thread::sleep(total - elapsed);
    }
}

/// Push one border-colour frame through hyprctl (repl first, legacy keyword
/// as fallback). Hyprland's `border` animation is on in hyprland.lua, so even
/// these discrete pushes get smoothed by the compositor.
pub fn set_borders(ramp: &Ramp) {
    let a = Ramp::hex8(ramp.accent, 1.0);
    let b = Ramp::hex8(ramp.accent_lo, 1.0);
    let inactive = Ramp::hex8(ramp.accent_muted, 1.0);
    let glow = Ramp::hex8(ramp.accent, 0.20);
    let lua = format!(
        "hl.config({{ general = {{ col = {{ \
           active_border = {{ colors = {{\"rgba({a})\",\"rgba({b})\"}}, angle = 45 }}, \
           inactive_border = \"rgba({inactive})\" }} }}, \
           decoration = {{ shadow = {{ color = \"rgba({glow})\" }} }} }})"
    );
    if !hyprctl(&["repl", &lua]) {
        let _ = hyprctl(&[
            "keyword",
            "general:col.active_border",
            &format!("rgba({a}) rgba({b}) 45deg"),
        ]);
    }
}

fn hyprctl(args: &[&str]) -> bool {
    std::process::Command::new("hyprctl")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiny_moves_skip_the_fade() {
        let a = Ramp::derive([0.2, 0.4, 0.9], 1.0);
        assert_eq!(step_count(&a, &a), 0);
        let b = Ramp::derive([1.0, 0.1, 0.2], 1.0);
        assert_eq!(step_count(&a, &b), 1);
    }

    #[test]
    fn transition_rule_mentions_every_property_we_recolor() {
        for prop in ["background-color", "color", "border-color"] {
            assert!(CSS_TRANSITION.contains(prop), "{prop} missing");
        }
        assert!(CSS_TRANSITION.contains("transition"));
    }
}
