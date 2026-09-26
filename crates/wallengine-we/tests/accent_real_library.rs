//! Accent extraction against the machine's real Wallpaper Engine library.
//!
//! Skips cleanly when nothing is installed, so CI (and a fresh checkout) stay
//! green — but on a real desktop this is the test that catches "the theme went
//! grey for every anime wallpaper" long before the UI does.

use wallengine_we::{dominant_color, rgb_to_hsv, scan_all};

#[test]
fn every_installed_wallpaper_yields_a_usable_accent() {
    let entries = scan_all();
    if entries.is_empty() {
        eprintln!("no Wallpaper Engine library installed; skipping");
        return;
    }

    let mut with_scheme = 0usize;
    let mut with_preview = 0usize;
    let mut derived = 0usize;

    for e in entries.iter().take(60) {
        if let Some(rgb) = e.project.scheme_color() {
            with_scheme += 1;
            assert!(
                rgb.iter().all(|c| (0.0..=1.0).contains(c)),
                "«{}» scheme color out of range: {rgb:?}",
                e.project.title
            );
        }
        let Some(preview) = e.preview.as_ref() else {
            continue;
        };
        with_preview += 1;
        let Some(rgb) = dominant_color(preview) else {
            // A preview we can't decode is fine (unsupported codec); the UI
            // falls back to the fixed accent.
            eprintln!("undecodable preview: {}", preview.display());
            continue;
        };
        derived += 1;
        assert!(
            rgb.iter().all(|c| c.is_finite() && (0.0..=1.0).contains(c)),
            "«{}» preview color out of range: {rgb:?}",
            e.project.title
        );
        let (_, _, v) = rgb_to_hsv(rgb[0], rgb[1], rgb[2]);
        assert!(
            v > 0.01,
            "«{}» preview color is pure black — the weighting collapsed",
            e.project.title
        );
    }

    // Previews are the fallback path; if almost none of them decode, the
    // dynamic theme silently degrades to the fixed accent everywhere.
    assert!(
        with_preview == 0 || derived * 4 >= with_preview * 3,
        "only {derived}/{with_preview} previews produced a color"
    );
    eprintln!(
        "{} entries · {with_scheme} declare a scheme color · {derived}/{with_preview} previews decoded",
        entries.len().min(60)
    );
}
