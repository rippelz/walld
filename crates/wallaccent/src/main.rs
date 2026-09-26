//! wallaccent — recolor the desktop from whatever wallpaper is on screen.
//!
//! The wallpaper's Wallpaper Engine `schemecolor` (or, failing that, the
//! dominant colour of its image) becomes the accent for waybar, kitty,
//! Hyprland borders, mako, rofi and optionally GTK.
//!
//! Designed to live alongside the `theme` CLI rather than fight it: themes
//! still own every colour, wallaccent only overrides the *accent* on top,
//! through separate generated files. `theme apply` runs `wallaccent apply` at
//! the end, so switching themes and switching wallpapers both land in a
//! consistent place.

mod ramp;
mod smooth;
mod source;
mod targets;

use ramp::Ramp;
use serde_json::{json, Value};
use source::{Found, Origin, Prefer};
use std::path::PathBuf;

const USAGE: &str = "\
wallaccent — desktop accent color from the current wallpaper

usage:
  wallaccent apply [--wallpaper PATH] [--color HEX] [--monitor NAME] [--dry-run]
  wallaccent status                 what's applied, and where it came from
  wallaccent on | off               enable / disable (off restores theme colors)
  wallaccent smooth on | off        fade between accents instead of snapping
  wallaccent set <HEX> | auto       pin a color, or follow the wallpaper again
  wallaccent targets [list|enable X|disable X]
  wallaccent config                 print the config file

options:
  --wallpaper PATH   use this wallpaper instead of the live one
  --color HEX        use this color instead of deriving one
  --monitor NAME     follow this output's wallpaper (default: first)
  --source auto|scheme|image
  --strength 0..1    how much of the wallpaper's saturation to keep
  --smooth | --instant   override the fade setting for this run
  --dry-run          report what would change, write nothing
  --quiet            only print errors

config: ~/.config/wallaccent/config.json
";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("apply");
    if cmd == "-h" || cmd == "--help" || cmd == "help" {
        print!("{USAGE}");
        return;
    }
    let code = match cmd {
        "apply" => cmd_apply(&args[1..]),
        "status" => cmd_status(),
        "on" => cmd_toggle(true),
        "off" => cmd_toggle(false),
        "smooth" => cmd_smooth(args.get(1).map(String::as_str)),
        "set" => cmd_set(args.get(1).map(String::as_str)),
        "auto" => cmd_set(Some("auto")),
        "targets" => cmd_targets(&args[1..]),
        "config" => {
            println!("{}", config_path().display());
            print!("{}", std::fs::read_to_string(config_path()).unwrap_or_default());
            0
        }
        other => {
            eprintln!("wallaccent: unknown command '{other}'\n");
            print!("{USAGE}");
            2
        }
    };
    std::process::exit(code);
}

// ── config ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct Config {
    enabled: bool,
    prefer: Prefer,
    /// Pinned colour; `None` follows the wallpaper.
    pinned: Option<[f32; 3]>,
    monitor: String,
    strength: f32,
    targets: Vec<String>,
    /// Fade waybar / Hyprland borders between accents instead of snapping.
    smooth: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled: true,
            prefer: Prefer::Auto,
            pinned: None,
            monitor: String::new(),
            strength: 1.0,
            targets: targets::DEFAULT_ON.iter().map(|s| s.to_string()).collect(),
            smooth: true,
        }
    }
}

fn config_dir() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| source::home().join(".config"))
        .join("wallaccent")
}

fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

fn load_config() -> Config {
    let mut c = Config::default();
    let Ok(text) = std::fs::read_to_string(config_path()) else {
        return c;
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        eprintln!("wallaccent: config.json isn't valid JSON — using defaults");
        return c;
    };
    if let Some(b) = v.get("enabled").and_then(Value::as_bool) {
        c.enabled = b;
    }
    if let Some(s) = v.get("source").and_then(Value::as_str) {
        c.prefer = parse_prefer(s).unwrap_or(Prefer::Auto);
    }
    c.pinned = v
        .get("pinned")
        .and_then(Value::as_str)
        .and_then(wallengine_we::accent::from_hex);
    if let Some(s) = v.get("monitor").and_then(Value::as_str) {
        c.monitor = s.to_string();
    }
    if let Some(f) = v.get("strength").and_then(Value::as_f64) {
        c.strength = (f as f32).clamp(0.0, 1.0);
    }
    if let Some(b) = v.get("smooth").and_then(Value::as_bool) {
        c.smooth = b;
    }
    if let Some(arr) = v.get("targets").and_then(Value::as_array) {
        let picked: Vec<String> = arr
            .iter()
            .filter_map(Value::as_str)
            .filter(|t| targets::ALL.contains(t))
            .map(str::to_string)
            .collect();
        c.targets = picked;
    }
    c
}

fn save_config(c: &Config) -> Result<(), String> {
    std::fs::create_dir_all(config_dir()).map_err(|e| e.to_string())?;
    let v = json!({
        "enabled": c.enabled,
        "source": match c.prefer {
            Prefer::Auto => "auto",
            Prefer::Scheme => "scheme",
            Prefer::Image => "image",
        },
        "pinned": c.pinned.map(wallengine_we::accent::to_hex),
        "monitor": c.monitor,
        "strength": c.strength,
        "smooth": c.smooth,
        "targets": c.targets,
    });
    std::fs::write(
        config_path(),
        serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}

fn parse_prefer(s: &str) -> Option<Prefer> {
    match s {
        "auto" => Some(Prefer::Auto),
        "scheme" => Some(Prefer::Scheme),
        "image" => Some(Prefer::Image),
        _ => None,
    }
}

// ── state (what we last applied — makes `status` honest) ───────────────────

fn state_path() -> PathBuf {
    config_dir().join("state.json")
}

fn save_state(found: &Found, ramp: &Ramp, cfg_strength: f32) {
    let v = json!({
        "monitor": found.monitor,
        "wallpaper": found.path.display().to_string(),
        "title": found.title,
        "origin": found.origin.label(),
        "base": wallengine_we::accent::to_hex(found.color),
        "strength": cfg_strength,
        "accent": Ramp::hex(ramp.accent),
    });
    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::write(
        state_path(),
        serde_json::to_string_pretty(&v).unwrap_or_default(),
    );
}

// ── commands ───────────────────────────────────────────────────────────────

fn cmd_apply(args: &[String]) -> i32 {
    let mut cfg = load_config();
    let mut wallpaper: Option<PathBuf> = None;
    let mut color: Option<[f32; 3]> = None;
    let mut dry = false;
    let mut quiet = false;
    let mut smooth: Option<bool> = None;

    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--wallpaper" => wallpaper = it.next().map(|s| source::expand(s)),
            "--color" => match it.next().and_then(|s| wallengine_we::accent::from_hex(s)) {
                Some(c) => color = Some(c),
                None => {
                    eprintln!("wallaccent: --color needs a hex value like #3C6FE0");
                    return 2;
                }
            },
            "--monitor" => cfg.monitor = it.next().cloned().unwrap_or_default(),
            "--source" => match it.next().and_then(|s| parse_prefer(s)) {
                Some(p) => cfg.prefer = p,
                None => {
                    eprintln!("wallaccent: --source must be auto, scheme or image");
                    return 2;
                }
            },
            "--strength" => {
                cfg.strength = it
                    .next()
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(cfg.strength)
                    .clamp(0.0, 1.0)
            }
            "--smooth" => smooth = Some(true),
            "--instant" => smooth = Some(false),
            "--dry-run" | "-n" => dry = true,
            "--quiet" | "-q" => quiet = true,
            other => {
                eprintln!("wallaccent: unknown option '{other}'");
                return 2;
            }
        }
    }

    // Disabled: stand every target down so the theme's own colours return.
    if !cfg.enabled {
        let n = stand_down(&cfg, dry);
        if !quiet {
            println!("wallaccent is off — {n} target(s) using theme colors");
        }
        return 0;
    }

    let found = match resolve(&cfg, wallpaper, color) {
        Ok(f) => f,
        Err(msg) => {
            // Not an error worth failing a `theme apply` over.
            if !quiet {
                println!("wallaccent: {msg} — leaving theme colors alone");
            }
            return 0;
        }
    };

    let ramp = Ramp::derive(found.color, cfg.strength);
    let smooth = smooth.unwrap_or(cfg.smooth);
    // What's on screen right now — the start point of a fade. Only worth
    // looking up when fading is even possible.
    let from = if smooth && !dry {
        load_last_ramp(&cfg.strength)
    } else {
        None
    };
    let mut changed = Vec::new();
    for t in &cfg.targets {
        match targets::apply(t, Some(&ramp), from.as_ref(), dry) {
            Ok(o) if o.changed => changed.push(format!("{} ({})", o.name, o.note)),
            Ok(_) => {}
            Err(e) => eprintln!("wallaccent: {t}: {e}"),
        }
    }
    if !dry {
        if let Some(f) = &from {
            // Steps waybar + borders from `f` to `ramp` over ~0.8s. Blocks
            // until the last frame: the process must outlive the fade.
            // (waybar's own reload was suppressed above.)
            smooth::run(f.clone(), ramp);
        }
        save_state(&found, &ramp, cfg.strength);
    }
    if !quiet {
        let what = if changed.is_empty() {
            "already current".to_string()
        } else {
            changed.join(", ")
        };
        println!(
            "{} {} from {} «{}» → {}",
            if dry { "would apply" } else { "accent" },
            Ramp::hex(ramp.accent),
            found.origin.label(),
            found.title,
            what
        );
    }
    0
}

/// Resolve the colour to use, in priority order: explicit `--color`, the
/// pinned colour, then the wallpaper itself.
fn resolve(
    cfg: &Config,
    wallpaper: Option<PathBuf>,
    color: Option<[f32; 3]>,
) -> Result<Found, String> {
    if let Some(c) = color.or(cfg.pinned) {
        return Ok(Found {
            monitor: cfg.monitor.clone(),
            path: PathBuf::new(),
            color: c,
            origin: Origin::Pinned,
            title: wallengine_we::accent::to_hex(c),
        });
    }
    let (monitor, path) = match wallpaper {
        Some(p) => (cfg.monitor.clone(), p),
        None => source::current_wallpaper(&cfg.monitor)
            .ok_or_else(|| "no wallpaper found (is walld running?)".to_string())?,
    };
    let ((color, origin), title) = source::color_for(&path, cfg.prefer)
        .ok_or_else(|| format!("no usable color in {}", path.display()))?;
    Ok(Found {
        monitor,
        path,
        color,
        origin,
        title,
    })
}

fn stand_down(cfg: &Config, dry: bool) -> usize {
    let mut n = 0;
    // Stand *every* target down, not just the enabled ones — a target the user
    // just disabled still has our file on disk.
    for t in targets::ALL {
        if targets::apply(t, None, None, dry).is_ok() {
            n += 1;
        }
    }
    let _ = cfg;
    n
}

/// The ramp behind the last applied accent (from state.json), when it was
/// derived from the wallpaper and strength hasn't moved since. `None` when
/// there's nothing sensible to fade from.
fn load_last_ramp(strength: &f32) -> Option<Ramp> {
    let v: Value = std::fs::read_to_string(state_path())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())?;
    if v.get("strength")
        .and_then(Value::as_f64)
        .map(|s| (s as f32 - strength).abs() > 0.01)
        .unwrap_or(true)
    {
        return None; // settings moved — old frames belong to a different ramp
    }
    let base = v
        .get("base")
        .and_then(Value::as_str)
        .and_then(wallengine_we::accent::from_hex)?;
    Some(Ramp::derive(base, *strength))
}

fn cmd_status() -> i32 {
    let cfg = load_config();
    println!(
        "wallaccent  {}",
        if cfg.enabled { "on" } else { "off" }
    );
    println!("  source    {}", match cfg.prefer {
        Prefer::Auto => "auto (scheme → image)",
        Prefer::Scheme => "WE scheme color only",
        Prefer::Image => "wallpaper image only",
    });
    match cfg.pinned {
        Some(c) => println!("  pinned    {}", wallengine_we::accent::to_hex(c)),
        None => println!("  pinned    no (follows the wallpaper)"),
    }
    println!(
        "  monitor   {}",
        if cfg.monitor.is_empty() {
            "first with a wallpaper"
        } else {
            &cfg.monitor
        }
    );
    println!("  strength  {:.2}", cfg.strength);
    println!("  smooth    {}", if cfg.smooth { "on (fades on recolor)" } else { "off" });
    println!("  targets   {}", cfg.targets.join(", "));

    if let Ok(text) = std::fs::read_to_string(state_path()) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("—").to_string();
            println!("\nlast applied");
            println!("  accent    {}", get("accent"));
            println!("  from      {} ({})", get("title"), get("origin"));
            println!("  wallpaper {}", get("wallpaper"));
        }
    }

    // A live check beats a cached one: show what the wallpaper says right now.
    if let Some((mon, path)) = source::current_wallpaper(&cfg.monitor) {
        println!("\nlive wallpaper");
        println!("  monitor   {}", if mon.is_empty() { "—" } else { &mon });
        println!("  path      {}", path.display());
        match source::color_for(&path, cfg.prefer) {
            Some(((c, origin), title)) => {
                let r = Ramp::derive(c, cfg.strength);
                println!(
                    "  color     {} → {} ({}, «{}»)",
                    wallengine_we::accent::to_hex(c),
                    Ramp::hex(r.accent),
                    origin.label(),
                    title
                );
            }
            None => println!("  color     none — theme colors would be kept"),
        }
    }
    0
}

fn cmd_toggle(on: bool) -> i32 {
    let mut cfg = load_config();
    cfg.enabled = on;
    if let Err(e) = save_config(&cfg) {
        eprintln!("wallaccent: {e}");
        return 1;
    }
    if on {
        cmd_apply(&[])
    } else {
        let n = stand_down(&cfg, false);
        println!("wallaccent off — {n} target(s) restored to theme colors");
        0
    }
}

fn cmd_smooth(arg: Option<&str>) -> i32 {
    let on = match arg {
        Some("on") | Some("true") | Some("1") => true,
        Some("off") | Some("false") | Some("0") => false,
        _ => {
            eprintln!("wallaccent: usage: wallaccent smooth on|off");
            return 2;
        }
    };
    let mut cfg = load_config();
    cfg.smooth = on;
    if let Err(e) = save_config(&cfg) {
        eprintln!("wallaccent: {e}");
        return 1;
    }
    println!(
        "smooth transitions {}",
        if on { "on — recolors fade over ~0.8s" } else { "off — recolors snap" }
    );
    0
}

fn cmd_set(arg: Option<&str>) -> i32 {
    let Some(arg) = arg else {
        eprintln!("wallaccent: usage: wallaccent set <#RRGGBB|auto>");
        return 2;
    };
    let mut cfg = load_config();
    if arg == "auto" {
        cfg.pinned = None;
    } else {
        match wallengine_we::accent::from_hex(arg) {
            Some(c) => cfg.pinned = Some(c),
            None => {
                eprintln!("wallaccent: '{arg}' is not a hex color");
                return 2;
            }
        }
    }
    if let Err(e) = save_config(&cfg) {
        eprintln!("wallaccent: {e}");
        return 1;
    }
    cmd_apply(&[])
}

fn cmd_targets(args: &[String]) -> i32 {
    let mut cfg = load_config();
    match args.first().map(String::as_str) {
        None | Some("list") => {
            for t in targets::ALL {
                let on = cfg.targets.iter().any(|x| x == t);
                println!("  [{}] {t}", if on { "x" } else { " " });
            }
            0
        }
        Some(op @ ("enable" | "disable")) => {
            let Some(name) = args.get(1) else {
                eprintln!("wallaccent: usage: wallaccent targets {op} <name>");
                return 2;
            };
            if !targets::ALL.contains(&name.as_str()) {
                eprintln!("wallaccent: no target '{name}' (have: {})", targets::ALL.join(", "));
                return 2;
            }
            cfg.targets.retain(|t| t != name);
            if op == "enable" {
                cfg.targets.push(name.clone());
            } else {
                // Put the theme's own colours back for the target being dropped.
                let _ = targets::apply(name, None, None, false);
            }
            if let Err(e) = save_config(&cfg) {
                eprintln!("wallaccent: {e}");
                return 1;
            }
            cmd_apply(&[])
        }
        Some(other) => {
            eprintln!("wallaccent: unknown targets command '{other}'");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests repoint XDG_CONFIG_HOME, which is process-global — they
    /// must not run at the same time as each other.
    static ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn config_round_trips() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("wallaccent-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &dir);

        assert!(load_config().enabled);
        let mut c = Config::default();
        c.enabled = false;
        c.prefer = Prefer::Image;
        c.pinned = wallengine_we::accent::from_hex("#3C6FE0");
        c.monitor = "DP-1".into();
        c.strength = 0.5;
        c.targets = vec!["waybar".into(), "kitty".into()];
        save_config(&c).unwrap();

        let back = load_config();
        assert!(!back.enabled);
        assert_eq!(back.prefer, Prefer::Image);
        assert_eq!(
            back.pinned.map(wallengine_we::accent::to_hex).as_deref(),
            Some("#3C6FE0")
        );
        assert_eq!(back.monitor, "DP-1");
        assert!((back.strength - 0.5).abs() < 1e-6);
        assert_eq!(back.targets, vec!["waybar".to_string(), "kitty".to_string()]);

        // Garbage must not stop the tool from running.
        std::fs::write(config_path(), "{{{").unwrap();
        assert!(load_config().enabled);

        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unknown_targets_in_config_are_dropped() {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("wallaccent-cfg2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        std::env::set_var("XDG_CONFIG_HOME", &dir);
        std::fs::create_dir_all(config_dir()).unwrap();
        std::fs::write(
            config_path(),
            r#"{"targets":["waybar","nonsense","kitty"]}"#,
        )
        .unwrap();
        assert_eq!(load_config().targets, vec!["waybar".to_string(), "kitty".to_string()]);
        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn explicit_color_wins_over_the_wallpaper() {
        let cfg = Config::default();
        let c = wallengine_we::accent::from_hex("#123456").unwrap();
        let f = resolve(&cfg, None, Some(c)).unwrap();
        assert_eq!(f.color, c);
        assert_eq!(f.origin, Origin::Pinned);
    }
}
