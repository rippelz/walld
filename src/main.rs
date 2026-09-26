//! walld — lightweight Hyprland wallpaper daemon.
//!
//! One layer-shell surface per output, EGL/GLES drawing, IPC on
//! $XDG_RUNTIME_DIR/walld.sock, SIGHUP = reload config with the configured
//! transition. Static walls are GL textures; switches can snap or do a
//! GPU diagonal wipe (old→new half-plane blend).

mod boot_still;
mod config;
mod effects;
mod image;
mod ipc;
mod present;
mod render;
mod session;
mod video;
mod video_mpv;
mod wayland;
mod we_runtime;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use calloop::channel::{self, Channel};
use calloop::generic::{FdWrapper, Generic};
use calloop::signals::{Event as SignalEvent, Signal, Signals};
use calloop::{EventLoop, Interest, LoopSignal, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{wl_compositor, wl_output, wl_registry, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use config::{FitMode, Transition, WalldConfig, WallpaperCfg};
use ipc::IpcCmd;
use wallengine_scene::{FitMode as SceneFit, SceneRuntime};
use wallengine_we::DecodedTex;
use wayland::{Output, OutputInfo, PendingWall, SurfaceState, Texture, WipeState, WipeWe, NAMESPACE};

/// Procedural noise map for VHS / TV-static when packages omit `util/noise`.
fn synth_noise_tex(n: u32) -> DecodedTex {
    let n = n.max(16);
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    let mut state = 0xA341316Cu32;
    for px in rgba.chunks_exact_mut(4) {
        state = state
            .wrapping_mul(1664525)
            .wrapping_add(1013904223);
        let v = (state >> 16) as u8;
        px[0] = v;
        px[1] = (state >> 8) as u8;
        px[2] = state as u8;
        px[3] = 255;
    }
    DecodedTex {
        width: n,
        height: n,
        content_width: n,
        content_height: n,
        texture_width: n,
        texture_height: n,
        format: wallengine_we::TexFormat::Argb8888,
        flags: 0,
        free_image: None,
        rgba,
        frames: Vec::new(),
        frame_times: Vec::new(),
        video_path: None,
    }
}

/// Decoded image result coming back from the worker thread.
struct DecodeResp {
    path: PathBuf,
    res: Result<image::Image, String>,
}

/// One Wallpaper Engine package bound to specific monitor(s).
///
/// walld keeps **multiple** of these so DP-1 and DP-2 can run different
/// wallpapers (and independent present/runtime state) at the same time.
/// The "active" bundle lives in `Daemon`'s `we_*` fields for the draw path;
/// the rest sit in `we_parked` and are rotated in each tick.
struct WeBundle {
    content: we_runtime::WeContent,
    dir: PathBuf,
    monitors: Vec<String>,
    present: present::WePresent,
    /// Per-output visual overrides (flip/zoom/fit/pan) when one package spans
    /// multiple displays. Key = output name (`DP-1`).
    present_mon: HashMap<String, present::WePresent>,
    started: Instant,
    layer_tex: Vec<Option<Texture>>,
    layer_video: Vec<Option<video::VideoDecoder>>,
    text_tex: Vec<Option<(Texture, u64)>>,
    particle_tex: Vec<Option<Texture>>,
    fx_progs: HashMap<usize, Vec<Option<glow::Program>>>,
    fx_tex: HashMap<String, Texture>,
    mask_tex: Vec<Option<Texture>>,
    phase_tex: Vec<Option<Texture>>,
}

fn bundle_monitors_is_all(monitors: &[String]) -> bool {
    monitors.is_empty() || monitors.iter().any(|m| m == "*")
}

/// Rate-limited "WE couldn't paint this output" logging.
///
/// `draw_we_on` runs at video frame rate, so logging every bail would flood the
/// log — but bailing *silently* is exactly how the wallpaper freezes on the last
/// committed frame while the decoder happily keeps pulling frames at full speed.
/// That combination (static screen, busy ffmpeg) is otherwise indistinguishable
/// from a healthy wallpaper, so it needs to leave a trace.
fn log_we_draw_bail(reason: &str) {
    use std::cell::RefCell;
    thread_local! {
        static LAST: RefCell<Option<(String, Instant)>> = RefCell::new(None);
    }
    LAST.with(|last| {
        let mut last = last.borrow_mut();
        let fresh = match last.as_ref() {
            Some((prev, at)) => prev != reason || at.elapsed() >= Duration::from_secs(5),
            None => true,
        };
        if fresh {
            log::warn!("WE draw skipped: {reason}");
            *last = Some((reason.to_string(), Instant::now()));
        }
    });
}

fn bundle_targets_name(monitors: &[String], name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    if bundle_monitors_is_all(monitors) {
        return true;
    }
    monitors
        .iter()
        .any(|m| m == name || m.eq_ignore_ascii_case(name))
}

struct Daemon {
    cfg: WalldConfig,
    /// Kept so the Wayland connection outlives EGL surfaces; reads go via WaylandSource.
    #[allow(dead_code)]
    conn: Connection,
    qh: QueueHandle<Daemon>,
    compositor: Option<wl_compositor::WlCompositor>,
    layer_shell: Option<zwlr_layer_shell_v1::ZwlrLayerShellV1>,
    outputs: HashMap<wayland_client::backend::ObjectId, Output>,
    /// wl_surface/layer_surface id → owning output id.
    surface_map: HashMap<wayland_client::backend::ObjectId, wayland_client::backend::ObjectId>,
    renderer: render::Renderer,
    /// Per-monitor wallpaper state from the last config read / IPC set.
    cfg_wallpapers: Vec<WallpaperCfg>,
    /// path → decoded pixels (small LRU, keeps theme switches instant).
    cache: VecDeque<(PathBuf, image::Image)>,
    in_flight: HashSet<PathBuf>,
    decode_tx: std::sync::mpsc::Sender<PathBuf>,
    loop_signal: Option<LoopSignal>,
    running: bool,
    /// Active engine scene (None = classic single-image mode).
    scene: Option<SceneRuntime>,
    scene_path: Option<PathBuf>,
    /// path → GPU texture for scene image layers.
    scene_tex: HashMap<PathBuf, Texture>,
    last_scene_tick: Instant,
    /// Wallpaper Engine content owned by the engine (video / we-scene).
    we_content: Option<we_runtime::WeContent>,
    /// Uploaded textures for WE image layers (index into scene.images).
    we_layer_tex: Vec<Option<Texture>>,
    /// Continuous ffmpeg streams for embedded MP4 layer textures (parallel).
    we_layer_video: Vec<Option<video::VideoDecoder>>,
    /// Text-layer textures + generation, re-uploaded when the string changes.
    we_text_tex: Vec<Option<(Texture, u64)>>,
    /// Particle sprite textures, parallel to runtime.particles.
    we_particle_tex: Vec<Option<Texture>>,
    /// Compiled WE effect-pass programs per layer index.
    /// One entry per authoring pass (same order as `effect_passes`/`passes`).
    /// `None` = compile failed — must still advance the index so later passes
    /// don't run the wrong program (was freezing fluids/shake on many scenes).
    we_fx_progs: std::collections::HashMap<usize, Vec<Option<glow::Program>>>,
    /// Sampler textures used by effect passes, by asset name.
    we_fx_tex: std::collections::HashMap<String, Texture>,
    /// Reusable, alias-safe effect render targets.
    we_fx_targets: effects::EffectTargets,
    /// Full-viewport scene RT used when a util/composelayer is present so the
    /// censor stack can sample real pixels (Wayland default FB copies are junk).
    we_scene_rt: Option<((u32, u32), (glow::Framebuffer, glow::Texture))>,
    /// Optional flow-mask textures parallel to we_layer_tex.
    we_mask_tex: Vec<Option<Texture>>,
    /// Optional phase textures parallel to we_layer_tex.
    we_phase_tex: Vec<Option<Texture>>,
    /// Wall-clock start for WE animation (not reset every frame).
    we_started: Instant,
    /// Latest compositor cursor in wallpaper UV 0..1 (y-down), for effects/scripts.
    cursor_uv: [f32; 2],
    /// Directory of the currently loaded WE package (for prop reload / identity).
    we_dir: Option<PathBuf>,
    /// Outputs that paint WE content. Empty or contains `"*"` → all monitors.
    /// Otherwise only matching `OutputInfo::name` values (e.g. `DP-1`).
    we_monitors: Vec<String>,
    /// Runtime presentation (pause, rate, fit, zoom, pan, flip).
    we_present: present::WePresent,
    /// Per-monitor visual overrides for the active WE package.
    we_present_mon: HashMap<String, present::WePresent>,
    /// Extra WE packages for other monitors (not currently in `we_*` fields).
    we_parked: Vec<WeBundle>,
    /// True from startup until `restore_we_session` has run. The boot `reload`
    /// paints hyprpaper.conf's classic walls first, and at that point no WE is
    /// live yet — so the accent hook would happily follow a wallpaper that is
    /// about to be replaced by the restored pack milliseconds later. Worse, it
    /// loses the race often: the classic image has to be decoded (multi-megapixel
    /// JPEG) while the WE pack reads a `schemecolor` straight out of project.json,
    /// so the stale refresh finishes last and wins. Suppress it and let the
    /// refresh at the end of `restore_we_session` be the one that lands.
    we_restore_pending: bool,
    /// Scene-editor GPU preview slot. Independent of desktop WE bundles so
    /// opening Edit never parks/stops the live wallpaper.
    editor_slot: Option<WeBundle>,
    /// Longest edge of the offscreen editor capture FBO.
    editor_max_edge: u32,
    /// Offscreen colour target for editor frames.
    editor_fbo: Option<((u32, u32), (glow::Framebuffer, glow::Texture))>,
    /// Scratch offscreen target for the outgoing-scene still during a WE wipe.
    /// `still_capture_we_for` renders the current WE frame into it (FBO route —
    /// window readback freezes WE video on AMD/Mesa) and hands the texture off
    /// to `load_we_package_ex`, which arms it as the wipe's old side.
    we_still_fbo: Option<((u32, u32), (glow::Framebuffer, glow::Texture))>,
    /// Set while `draw_we_on` should paint into `we_still_fbo` and skip the
    /// window swap (capture mode, same trick as `editor_capture_active`).
    we_still_capture_pending: bool,
    /// The still texture the pending capture produced (handed to the wipe).
    we_still_ready: Option<glow::Texture>,
    /// Bumped every time a new editor PNG is written.
    editor_frame_gen: u64,
    /// When true, `draw_we_on` paints into the editor FBO (GPU capture) instead
    /// of the Wayland surface and skips swap.
    editor_capture_active: bool,
    /// Monitors that still need a boot-still PNG after the next good WE frames.
    boot_capture_pending: HashSet<String>,
    /// Don't capture the first instant of black/loading — wait for real content.
    boot_capture_after: Option<Instant>,
    /// Successful WE presents per monitor since arming (need a few before save).
    boot_capture_frames: HashMap<String, u32>,
}

impl Daemon {
    fn new(cfg: WalldConfig, conn: Connection, qh: QueueHandle<Daemon>, decode_tx: std::sync::mpsc::Sender<PathBuf>) -> Result<Self, String> {
        let renderer = render::Renderer::new(&conn)?;
        Ok(Daemon {
            cfg,
            conn,
            qh,
            compositor: None,
            layer_shell: None,
            outputs: HashMap::new(),
            surface_map: HashMap::new(),
            renderer,
            cfg_wallpapers: Vec::new(),
            cache: VecDeque::new(),
            in_flight: HashSet::new(),
            decode_tx,
            loop_signal: None,
            running: true,
            scene: None,
            scene_path: None,
            scene_tex: HashMap::new(),
            last_scene_tick: Instant::now(),
            we_content: None,
            we_layer_tex: Vec::new(),
            we_layer_video: Vec::new(),
            we_text_tex: Vec::new(),
            we_particle_tex: Vec::new(),
            we_fx_progs: std::collections::HashMap::new(),
            we_fx_tex: std::collections::HashMap::new(),
            we_fx_targets: effects::EffectTargets::default(),
            we_scene_rt: None,
            we_mask_tex: Vec::new(),
            we_phase_tex: Vec::new(),
            we_started: Instant::now(),
            cursor_uv: [0.5, 0.5],
            we_dir: None,
            we_monitors: Vec::new(),
            we_present: present::WePresent::default(),
            we_present_mon: HashMap::new(),
            we_parked: Vec::new(),
            we_restore_pending: !session::load().slots.is_empty(),
            editor_slot: None,
            editor_max_edge: 720,
            editor_fbo: None,
            we_still_fbo: None,
            we_still_capture_pending: false,
            we_still_ready: None,
            editor_frame_gen: 0,
            editor_capture_active: false,
            boot_capture_pending: HashSet::new(),
            boot_capture_after: None,
            boot_capture_frames: HashMap::new(),
        })
    }

    /// Resolve a workshop id or absolute path to a wallpaper directory.
    fn resolve_we_dir(path: &Path) -> Result<PathBuf, String> {
        if path.is_dir() {
            return Ok(path.to_path_buf());
        }
        if path.is_file() && path.file_name().and_then(|s| s.to_str()) == Some("project.json") {
            return path
                .parent()
                .map(|p| p.to_path_buf())
                .ok_or_else(|| "bad project.json path".into());
        }
        let id = path.to_string_lossy();
        let cand = wallengine_we::workshop_dir().join(id.as_ref());
        if cand.is_dir() {
            Ok(cand)
        } else {
            Err(format!("not a WE wallpaper dir: {}", path.display()))
        }
    }

    fn workshop_id_for_dir(dir: &Path) -> String {
        dir.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string())
    }

    /// Feed decoder clocks into SceneScript video handles before scripts run.
    fn sync_script_videos_pre_tick(&mut self) {
        let mut layers = Vec::new();
        for (i, dec) in self.we_layer_video.iter().enumerate() {
            let Some(dec) = dec else { continue };
            let name = match &self.we_content {
                Some(we_runtime::WeContent::Scene { runtime }) => runtime
                    .images
                    .get(i)
                    .map(|l| l.name.clone())
                    .unwrap_or_default(),
                _ => continue,
            };
            if name.is_empty() {
                continue;
            }
            let ctl = dec.control_snapshot();
            layers.push((name, ctl.time, ctl.duration, ctl.playing));
        }
        if layers.is_empty() {
            return;
        }
        if let Some(we_runtime::WeContent::Scene { runtime }) = self.we_content.as_mut() {
            runtime.sync_script_video_times(&layers);
        }
    }

    /// Apply getVideoTexture() play/pause/seek from the last script tick.
    fn apply_script_video_commands(&mut self) {
        let cmds = match self.we_content.as_mut() {
            Some(we_runtime::WeContent::Scene { runtime }) => {
                runtime.drain_script_video_commands()
            }
            _ => return,
        };
        for cmd in cmds {
            let Some(idx) = (match &self.we_content {
                Some(we_runtime::WeContent::Scene { runtime }) => runtime
                    .images
                    .iter()
                    .position(|l| l.name == cmd.name),
                _ => None,
            }) else {
                continue;
            };
            if let Some(Some(dec)) = self.we_layer_video.get(idx) {
                dec.apply_script_control(cmd.playing, cmd.r#loop, cmd.seek);
            }
        }
    }

    /// Poll global cursor (Hyprland) and map into 0..1 UV on the primary WE surface.
    fn poll_cursor_uv(&mut self) {
        // hyprctl cursorpos → "x,y" in global layout coordinates.
        let out = std::process::Command::new("hyprctl")
            .args(["cursorpos", "-j"])
            .output()
            .ok()
            .or_else(|| {
                std::process::Command::new("hyprctl")
                    .arg("cursorpos")
                    .output()
                    .ok()
            });
        let Some(out) = out else { return };
        if !out.status.success() {
            return;
        }
        let s = String::from_utf8_lossy(&out.stdout);
        let (cx, cy) = if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
            // JSON: {"x":..,"y":..} or [x,y]
            let x = v
                .get("x")
                .and_then(|n| n.as_f64())
                .or_else(|| v.as_array().and_then(|a| a.first()).and_then(|n| n.as_f64()))
                .unwrap_or(0.0) as f32;
            let y = v
                .get("y")
                .and_then(|n| n.as_f64())
                .or_else(|| v.as_array().and_then(|a| a.get(1)).and_then(|n| n.as_f64()))
                .unwrap_or(0.0) as f32;
            (x, y)
        } else {
            // Plain "x,y" or "x y"
            let parts: Vec<f32> = s
                .split(|c: char| c == ',' || c.is_whitespace())
                .filter_map(|p| p.trim().parse().ok())
                .collect();
            if parts.len() < 2 {
                return;
            }
            (parts[0], parts[1])
        };

        // Prefer the largest output (usually the main wallpaper surface).
        let mut best: Option<(i32, i32, u32, u32)> = None; // x,y,w,h in global space
        // Hyprland monitor positions: parse from hyprctl monitors -j once is heavy;
        // approximate: map into first ready surface using layout order from info.
        // Better: hyprctl monitors -j with x/y. Cache lightly by calling when needed.
        if let Ok(mout) = std::process::Command::new("hyprctl")
            .args(["monitors", "-j"])
            .output()
        {
            if let Ok(arr) = serde_json::from_slice::<serde_json::Value>(&mout.stdout) {
                if let Some(list) = arr.as_array() {
                    for m in list {
                        let x = m.get("x").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        let y = m.get("y").and_then(|v| v.as_i64()).unwrap_or(0) as i32;
                        let w = m.get("width").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let h = m.get("height").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        if w == 0 || h == 0 {
                            continue;
                        }
                        if cx >= x as f32
                            && cy >= y as f32
                            && cx < (x as f32 + w as f32)
                            && cy < (y as f32 + h as f32)
                        {
                            best = Some((x, y, w, h));
                            break;
                        }
                        // Keep largest as fallback.
                        if best.map(|(_, _, bw, bh)| bw * bh).unwrap_or(0) < w * h {
                            best = Some((x, y, w, h));
                        }
                    }
                }
            }
        }
        if let Some((x, y, w, h)) = best {
            let u = ((cx - x as f32) / w as f32).clamp(0.0, 1.0);
            let v = ((cy - y as f32) / h as f32).clamp(0.0, 1.0);
            self.cursor_uv = [u, v];
        }
    }

    /// Bind globals we care about (initial list and hotplugs alike).
    fn bind_global(&mut self, registry: &wl_registry::WlRegistry, name: u32, interface: &str, version: u32) {
        match interface {
            "wl_compositor" => {
                self.compositor =
                    Some(registry.bind::<wl_compositor::WlCompositor, (), _>(name, version.min(6), &self.qh, ()));
            }
            "zwlr_layer_shell_v1" => {
                self.layer_shell = Some(
                    registry.bind::<zwlr_layer_shell_v1::ZwlrLayerShellV1, (), _>(name, version.min(4), &self.qh, ()),
                );
            }
            "wl_output" => {
                let out = registry.bind::<wl_output::WlOutput, (), _>(name, version.min(4), &self.qh, ());
                log::debug!("output global added: {name}");
                self.outputs.insert(
                    out.id(),
                    Output {
                        wl_output: out,
                        global_name: name,
                        info: OutputInfo::default(),
                        surface: None,
                        current: None,
                        current_path: None,
                        fit: FitMode::Cover,
                        pending_path: None,
                        pending_transition: Transition::Snap,
                        wipe: None,
                        wipe_we: None,
                        wipe_pending_first: false,
                    },
                );
            }
            _ => {}
        }
        // A new output may already have everything it needs by the time
        // globals are processed; try creating its surface on every change.
        let ids: Vec<_> = self.outputs.keys().cloned().collect();
        for id in ids {
            self.ensure_surface(id);
        }
    }

    /// Create the layer-shell surface for an output once everything is known.
    fn ensure_surface(&mut self, out_id: wayland_client::backend::ObjectId) {
        let (Some(compositor), Some(shell)) = (self.compositor.clone(), self.layer_shell.clone()) else {
            return;
        };
        let Some(out) = self.outputs.get_mut(&out_id) else { return };
        if out.surface.is_some() || !out.info.ready() {
            return;
        }
        let name = out.info.name.clone();
        let wl_surface = compositor.create_surface(&self.qh, ());
        let layer = shell.get_layer_surface(
            &wl_surface,
            Some(&out.wl_output),
            zwlr_layer_shell_v1::Layer::Background,
            NAMESPACE.to_string(),
            &self.qh,
            (),
        );
        use zwlr_layer_surface_v1::Anchor;
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(-1); // ignore exclusive zones: true wallpaper
        layer.set_keyboard_interactivity(zwlr_layer_surface_v1::KeyboardInteractivity::None);
        layer.set_size(0, 0); // let the compositor hand us the full output size
        self.surface_map.insert(layer.id(), out_id.clone());
        self.surface_map.insert(wl_surface.id(), out_id.clone());
        out.surface = Some(SurfaceState {
            wl_surface: wl_surface.clone(),
            layer,
            egl_window: None,
            width: 0,
            height: 0,
            committed: false,
        });
        wl_surface.commit(); // triggers configure
        log::info!("output {name}: layer surface created");
        self.apply_config_to(out_id, None);
    }

    /// (Re)load config files and push wallpapers to outputs.
    fn reload(&mut self, transition_override: Option<Transition>) {
        self.cfg = config::load_global();
        if let Some(scene) = self.cfg.scene.clone() {
            match self.load_scene(&scene) {
                Ok(()) => {
                    log::info!("config: scene {}", scene.display());
                    return;
                }
                Err(e) => log::warn!("config scene failed ({e}); falling back to images"),
            }
        }
        // Classic per-monitor images from hyprpaper.conf
        if self.scene.is_some() {
            self.clear_scene();
        }
        let walls = config::read_hyprpaper_conf(&self.cfg.hyprpaper_conf);
        log::info!(
            "config: {} wallpaper block(s) from {} (transition={:?})",
            walls.len(),
            self.cfg.hyprpaper_conf.display(),
            transition_override.unwrap_or(self.cfg.transition)
        );
        self.cfg_wallpapers = walls;
        let ids: Vec<_> = self.outputs.keys().cloned().collect();
        for id in ids {
            self.apply_config_to(id, transition_override);
        }
        // SIGHUP / explicit reload: when no WE or scene owns the screen, the
        // classic walls just applied are the new wallpaper — follow them too.
        // At boot a saved WE session counts as owning the screen even though it
        // is not loaded yet (see `we_restore_pending`).
        if self.we_content.is_none()
            && self.we_parked.is_empty()
            && self.scene.is_none()
            && !self.we_restore_pending
        {
            self.maybe_accent_refresh();
        }
    }

    /// Snapshot live WE layout to `~/.config/walld/session.json` so reboot
    /// restores workshop wallpapers (hyprpaper.conf only covers static images).
    fn save_we_session(&self) {
        // Editor preview slot is never part of the desktop session.
        let mut slots: Vec<(Vec<String>, PathBuf)> = Vec::new();
        if self.we_content.is_some() {
            if let Some(dir) = self.we_dir.clone() {
                let mons = if self.we_monitors_is_all() {
                    vec!["*".into()]
                } else {
                    self.we_monitors.clone()
                };
                if !mons.is_empty() {
                    slots.push((mons, dir));
                }
            }
        }
        for b in &self.we_parked {
            let mons = if bundle_monitors_is_all(&b.monitors) {
                vec!["*".into()]
            } else {
                b.monitors.clone()
            };
            if !mons.is_empty() {
                slots.push((mons, b.dir.clone()));
            }
        }
        let sess = session::from_live_slots(slots);
        if sess.slots.is_empty() {
            session::clear();
            log::debug!("session: cleared (no WE active)");
            return;
        }
        match session::save(&sess) {
            Ok(()) => log::info!(
                "session: saved {} WE slot(s) → {}",
                sess.slots.len(),
                session::session_path().display()
            ),
            Err(e) => log::warn!("session: save failed: {e}"),
        }
    }

    /// Reload last WE layout after classic walls (boot / daemon restart).
    fn restore_we_session(&mut self) {
        // Boot's one-shot accent suppression ends here, however this returns:
        // from now on a reload really is the last word on the wallpaper.
        let suppressed = self.we_restore_pending;
        self.we_restore_pending = false;
        let sess = session::load();
        if sess.slots.is_empty() {
            log::debug!("session: nothing to restore");
            // The session emptied out between startup and here, so the classic
            // walls are the wallpaper after all and nothing has accented them.
            if suppressed {
                self.maybe_accent_refresh();
            }
            return;
        }
        log::info!(
            "session: restoring {} WE slot(s) from {}",
            sess.slots.len(),
            session::session_path().display()
        );
        for slot in &sess.slots {
            let mon = session::mon_arg(&slot.monitors);
            match self.load_we_package_on(&slot.path, &mon) {
                Ok(()) => log::info!(
                    "session: restored «{}» on {mon}",
                    slot.path
                        .file_name()
                        .map(|s| s.to_string_lossy())
                        .unwrap_or_default()
                ),
                Err(e) => log::warn!(
                    "session: failed {} on {mon}: {e}",
                    slot.path.display()
                ),
            }
        }
        // Re-save so dropped/missing packages are pruned from the file.
        self.save_we_session();
        // After WE is up, refresh boot stills so next login matches this pack.
        self.arm_boot_capture();
        // Login wallpaper switch: accent should already match the restored pack.
        self.maybe_accent_refresh();
    }

    /// Paint last-session stills immediately (before WE load) so the desktop
    /// never shows Hyprland's default bg while workshop content spins up.
    fn apply_boot_stills(&mut self) {
        let ids: Vec<_> = self.outputs.keys().cloned().collect();
        let mut n = 0u32;
        for id in ids {
            let name = match self.outputs.get(&id) {
                Some(o) if !o.info.name.is_empty() => o.info.name.clone(),
                _ => continue,
            };
            let Some(path) = boot_still::path_if_exists(&name) else {
                continue;
            };
            log::info!(
                "boot-still: painting {} from {}",
                name,
                path.display()
            );
            self.queue_wallpaper(
                id,
                PendingWall {
                    path,
                    transition: Transition::Snap,
                },
            );
            n += 1;
        }
        if n == 0 {
            log::debug!("boot-still: none on disk yet");
        } else {
            log::info!("boot-still: queued {n} monitor still(s)");
        }
    }

    /// Schedule GPU capture of live wallpaper → boot still PNGs.
    fn arm_boot_capture(&mut self) {
        // GPU capture is currently suppressed: sampling textures / FBO readback
        // after the present path was freezing WE video on AMD+Mesa/Wayland.
        // Classic image walls still write boot stills via on_decoded.
        // For WE, seed stills from workshop preview / ffmpeg offline instead.
        self.boot_capture_pending.clear();
        self.boot_capture_frames.clear();
        self.boot_capture_after = None;
        log::debug!("boot-still: GPU capture arm suppressed");
    }

    /// True when WE can paint a real frame (not empty/black clear).
    fn we_has_drawable_frame(&self) -> bool {
        match &self.we_content {
            Some(we_runtime::WeContent::Video { .. }) => self
                .we_layer_tex
                .first()
                .is_some_and(|t| t.is_some()),
            Some(we_runtime::WeContent::Scene { .. }) => {
                // Scene images upload at load; empty draw_list still "draws".
                true
            }
            None => false,
        }
    }

    /// After swap, maybe grab a boot still via offscreen FBO (never window readback).
    /// Window glReadPixels freezes subsequent presents on Mesa/AMD + Wayland.
    fn try_boot_capture_after_frame(
        &mut self,
        out_id: &wayland_client::backend::ObjectId,
    ) {
        if self.editor_capture_active {
            return;
        }
        let name = match self.outputs.get(out_id) {
            Some(o) if !o.info.name.is_empty() => o.info.name.clone(),
            _ => return,
        };
        if !self.boot_capture_pending.contains(&name) {
            return;
        }
        if self
            .boot_capture_after
            .is_some_and(|t| Instant::now() < t)
        {
            return;
        }
        if !self.we_has_drawable_frame() {
            return;
        }
        let n = self.boot_capture_frames.entry(name.clone()).or_insert(0);
        *n = n.saturating_add(1);
        // Wait for a few presents so video/scene isn't still ramping.
        if *n < 8 {
            return;
        }
        // Only attempt once per arm — never retry every frame.
        self.boot_capture_pending.remove(&name);
        self.boot_capture_frames.remove(&name);
        if self.boot_capture_pending.is_empty() {
            self.boot_capture_after = None;
        }

        // Sample live video/layer texture into an offscreen FBO.
        let (src_tex, tw, th) = if let Some(Some(t)) = self.we_layer_tex.first() {
            (t.tex, t.w, t.h)
        } else {
            log::debug!("boot-still: skip {name} (no layer tex to sample)");
            return;
        };
        if tw == 0 || th == 0 {
            return;
        }
        let long = tw.max(th);
        let (cw, ch) = if long > 1920 {
            let s = 1920.0 / long as f32;
            (
                ((tw as f32) * s).round().max(1.0) as u32,
                ((th as f32) * s).round().max(1.0) as u32,
            )
        } else {
            (tw, th)
        };
        let Some((fbo, fbo_tex)) = self.renderer.create_target(cw, ch) else {
            log::warn!("boot-still: FBO create failed for {name}");
            return;
        };
        self.renderer
            .bind_draw_target(Some((fbo, cw, ch)), cw as i32, ch as i32, true);
        self.renderer.clear_bound([0.0, 0.0, 0.0, 1.0]);
        self.renderer
            .draw_blit(cw as i32, ch as i32, src_tex, tw, th, FitMode::Cover);
        let rgba = self.renderer.read_rgba(fbo, cw, ch);
        self.renderer.delete_target(fbo, fbo_tex);
        // Restore wallpaper surface GL state for the next frame.
        if let Some(out) = self.outputs.get(out_id) {
            if let Some(s) = out.surface.as_ref() {
                if let Some(win) = s.egl_window.as_ref() {
                    let _ = self.renderer.attach_window(win);
                }
            }
        }
        if let Err(e) = boot_still::save_rgba(&name, cw, ch, &rgba) {
            log::warn!("boot-still: capture {name}: {e}");
        }
    }

    /// Persist classic-image boot stills (no GPU needed).
    fn save_boot_still_from_image(&self, monitor: &str, img: &image::Image) {
        if monitor.is_empty() {
            return;
        }
        if let Err(e) = boot_still::save_image(monitor, img) {
            log::warn!("boot-still: static {monitor}: {e}");
        }
    }

    /// Queue the configured wallpaper for one output (skipped when unchanged).
    fn apply_config_to(&mut self, out_id: wayland_client::backend::ObjectId, transition_override: Option<Transition>) {
        let Some(name) = self.outputs.get(&out_id).map(|o| o.info.name.clone()) else {
            return;
        };
        if name.is_empty() {
            return;
        }
        // WE owns this display — don't stomp it with the static hyprpaper path.
        if self.we_owns_monitor(&name) {
            let _ = self.draw_output(out_id);
            return;
        }
        let Some(w) = self.cfg_wallpapers.iter().find(|w| w.monitor == name) else {
            return;
        };
        let transition = transition_override.unwrap_or(self.cfg.transition);
        let out = self.outputs.get_mut(&out_id).unwrap();
        let same = out.current_path.as_deref() == Some(w.path.as_path()) && out.pending_path.is_none();
        out.fit = w.fit;
        if same {
            // Path unchanged — only fit mode may have moved; redraw statically.
            self.draw_output(out_id);
            return;
        }
        self.queue_wallpaper(out_id, PendingWall { path: w.path.clone(), transition });
    }

    /// IPC `set`/`snap`/`wipe` — monitor is a name or "*".
    fn set_wallpaper(&mut self, monitor: &str, path: &std::path::Path, transition_override: Option<Transition>) {
        // Don't kill WE on *other* monitors when setting a classic image on one.
        if monitor == "*" {
            if self.we_content.is_some() || !self.we_parked.is_empty() {
                self.clear_we();
            }
        } else {
            self.detach_we_monitor(monitor);
        }
        // Classic image path exits engine scene mode.
        if self.scene.is_some() {
            self.clear_scene();
        }
        let transition = transition_override.unwrap_or(self.cfg.transition);
        // Keep the in-memory per-monitor table consistent for status/reload.
        if monitor != "*" {
            if let Some(w) = self.cfg_wallpapers.iter_mut().find(|w| w.monitor == monitor) {
                w.path = path.to_path_buf();
            } else {
                self.cfg_wallpapers.push(WallpaperCfg {
                    monitor: monitor.to_string(),
                    path: path.to_path_buf(),
                    fit: FitMode::Cover,
                });
            }
        }
        let ids: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| monitor == "*" || o.info.name == monitor)
            .map(|(id, _)| id.clone())
            .collect();
        if ids.is_empty() {
            log::warn!("set: no output named {monitor}");
        }
        for id in ids {
            self.queue_wallpaper(id, PendingWall { path: path.to_path_buf(), transition });
        }
        // Classic image replaced WE on some/all displays — persist the new layout.
        self.save_we_session();
    }

    /// Recolor the desktop after the visible wallpaper changed.
    ///
    /// This is the "live" half of desktop accenting: whatever caused the
    /// switch (wallstudio, `walld ctl`, theme apply, session restore), the
    /// accent follows. `wallaccent` reads walld's own status to find what's
    /// on screen, so no arguments are needed; it no-ops when the color
    /// didn't move. Detached: the draw loop never waits on it.
    fn maybe_accent_refresh(&self) {
        if !self.cfg.accent_on_change {
            return;
        }
        // Resolved by path, not by name: the session that starts walld rarely
        // has ~/.local/bin on PATH, so a bare spawn would silently never fire.
        let Some(bin) = wallengine_we::wallaccent_binary() else {
            // Missing wallaccent is fine — it's an optional companion tool.
            log::debug!("accent: wallaccent not installed — skipping");
            return;
        };
        match std::process::Command::new(&bin)
            .args(["apply", "--quiet"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(mut child) => {
                // Never block the draw loop; reap on a thread so rapid
                // wallpaper switches can't pile up zombie children.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                log::info!("accent: refreshing from the new wallpaper");
            }
            Err(e) => log::warn!("accent: could not run {}: {e}", bin.display()),
        }
    }

    /// Get `pending` on screen: instant if cached, otherwise via the decode thread.
    fn queue_wallpaper(&mut self, out_id: wayland_client::backend::ObjectId, pending: PendingWall) {
        // Cache hit → skip the worker entirely.
        if let Some(idx) = self.cache.iter().position(|(p, _)| *p == pending.path) {
            let (_, img) = self.cache.remove(idx).unwrap();
            log::debug!("cache hit: {}", pending.path.display());
            self.on_decoded(out_id, pending.path, pending.transition, img);
            return;
        }
        if let Some(out) = self.outputs.get_mut(&out_id) {
            let old = out.current_path.as_deref().map(|p| p.display().to_string()).unwrap_or_default();
            log::info!("{}: {old} -> {} ({:?})", out.info.name, pending.path.display(), pending.transition);
            out.pending_path = Some(pending.path.clone());
            out.pending_transition = pending.transition;
        }
        if self.in_flight.insert(pending.path.clone()) {
            let _ = self.decode_tx.send(pending.path);
        }
    }

    /// Worker thread finished decoding `path`; every output waiting on it draws.
    fn handle_decode_result(&mut self, resp: DecodeResp) {
        let DecodeResp { path, res } = resp;
        self.in_flight.remove(&path);
        let img = match res {
            Ok(img) => img,
            Err(e) => {
                log::warn!("decode failed for {}: {e}", path.display());
                for out in self.outputs.values_mut() {
                    if out.pending_path.as_deref() == Some(path.as_path()) {
                        out.pending_path = None;
                    }
                }
                return;
            }
        };
        log::debug!("decoded {} ({}x{})", path.display(), img.width, img.height);
        let waiting: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| o.pending_path.as_deref() == Some(path.as_path()))
            .map(|(id, _)| id.clone())
            .collect();
        if waiting.is_empty() {
            // preload-only request: park in cache for the next switch
            self.cache_push(path, img);
            return;
        }
        // First output consumes the pixels; any others take a cache copy.
        let mut iter = waiting.into_iter();
        let first = iter.next().unwrap();
        let first_transition = self.outputs[&first].pending_transition;
        if iter.len() > 0 {
            let clone = image::Image { width: img.width, height: img.height, rgba: img.rgba.clone() };
            self.cache_push(path.clone(), clone);
        }
        self.on_decoded(first, path.clone(), first_transition, img);
        for id in iter {
            if let Some(idx) = self.cache.iter().position(|(p, _)| *p == path) {
                let (_, img2) = self.cache.remove(idx).unwrap();
                let t = self.outputs[&id].pending_transition;
                self.on_decoded(id, path.clone(), t, img2);
            }
        }
    }

    fn cache_push(&mut self, path: PathBuf, img: image::Image) {
        self.cache.retain(|(p, _)| *p != path);
        self.cache.push_back((path, img));
        while self.cache.len() > 2 {
            self.cache.pop_front();
        }
    }

    /// Decoded pixels arrived: upload texture + start the requested transition.
    fn on_decoded(&mut self, out_id: wayland_client::backend::ObjectId, path: PathBuf, transition: Transition, img: image::Image) {
        let tex = self.renderer.upload_rgba(&img.rgba, img.width, img.height);
        let new = Texture {
            tex,
            w: img.width,
            h: img.height,
            uv_scale: (1.0, 1.0),
        };
        // "Old" is either the live wallpaper or the wipe's in-flight target.
        let old = {
            let Some(out) = self.outputs.get_mut(&out_id) else { return };
            out.pending_path = None;
            out.current_path = Some(path);
            if let Some(wipe) = out.wipe.take() {
                self.renderer.delete_texture(wipe.old.tex);
                Some(wipe.new)
            } else {
                out.current.take()
            }
        };
        // Boot still for classic images (WE captures via GPU after first frames).
        let mon_name = self
            .outputs
            .get(&out_id)
            .map(|o| o.info.name.clone())
            .unwrap_or_default();
        if !mon_name.is_empty() {
            self.save_boot_still_from_image(&mon_name, &img);
        }

        match transition {
            Transition::Snap => {
                if let Some(old) = old {
                    self.renderer.delete_texture(old.tex);
                }
                if let Some(out) = self.outputs.get_mut(&out_id) {
                    out.current = Some(new);
                }
            }
            Transition::Wipe => {
                match old {
                    Some(old_tex) => {
                        let (wipe_ms, feather) = (self.cfg.wipe_ms, self.cfg.wipe_feather_px);
                        if let Some(out) = self.outputs.get_mut(&out_id) {
                            out.wipe = Some(WipeState {
                                old: old_tex,
                                new,
                                start: Instant::now(),
                                dur_ms: wipe_ms,
                                feather_px: feather,
                            });
                        }
                    }
                    None => {
                        // Nothing on screen to wipe from — just snap on.
                        if let Some(out) = self.outputs.get_mut(&out_id) {
                            out.current = Some(new);
                        }
                    }
                }
            }
        }
        self.draw_output(out_id);
    }

    /// Draw one output's current state (static frame or wipe at current time).
    /// Returns true if an animation is still running.
    fn draw_output(&mut self, out_id: wayland_client::backend::ObjectId) -> bool {
        // Rotate the matching per-monitor WE bundle into the active slot first.
        // Editor preview lives in `editor_slot` and never steals desktop monitors.
        if self.ensure_we_active_for_output(&out_id) {
            return self.draw_we_on(out_id);
        }
        // Engine scene mode takes priority over classic single-image wallpapers.
        if self.scene.is_some() {
            return self.draw_scene_on(out_id);
        }
        let Some(out) = self.outputs.get_mut(&out_id) else { return false };
        let Some(s) = out.surface.as_mut() else { return false };
        let Some(win) = s.egl_window.as_ref() else { return false };
        if s.width == 0 || s.height == 0 {
            return false;
        }
        let (w, h) = (s.width as i32, s.height as i32);
        if self.renderer.attach_window(win).is_err() {
            return false;
        }

        let fit = out.fit;
        let mut animating = false;
        if let Some(wipe) = out.wipe.as_ref() {
            let elapsed = wipe.start.elapsed().as_secs_f32() * 1000.0;
            let t = (elapsed / wipe.dur_ms.max(1) as f32).clamp(0.0, 1.0);
            let progress = wayland::ease_out_cubic(t);
            self.renderer.draw_wipe(
                w,
                h,
                wipe.old.tex,
                wipe.old.w,
                wipe.old.h,
                wipe.new.tex,
                wipe.new.w,
                wipe.new.h,
                fit,
                progress,
                wipe.feather_px,
            );
            animating = t < 1.0;
        } else if let Some(cur) = out.current.as_ref() {
            self.renderer.draw_blit(w, h, cur.tex, cur.w, cur.h, fit);
        }

        self.renderer.swap();
        s.wl_surface.commit();
        s.committed = true;

        if !animating {
            // Wipe finished (or static draw): settle into the new wallpaper.
            if let Some(wipe) = self.outputs.get_mut(&out_id).unwrap().wipe.take() {
                self.renderer.delete_texture(wipe.old.tex);
                let out = self.outputs.get_mut(&out_id).unwrap();
                out.current = Some(wipe.new);
            }
        }
        animating
    }

    /// Advance all wipes one frame. Returns Some(next tick) if still animating.
    fn tick_animations(&mut self) -> Option<Instant> {
        let mut next: Option<Instant> = None;

        // WE video / animated scene content — tick every per-monitor bundle.
        // Desktop always runs; the editor slot is ticked/captured separately so
        // opening Edit never freezes the live wallpaper.
        if self.any_we_needs_anim() {
            let raw_dt = self.last_scene_tick.elapsed().as_secs_f32();
            self.last_scene_tick = Instant::now();
            // Interactivity: global cursor → wallpaper UV for scripts + effects.
            self.poll_cursor_uv();

            let mut max_fps = self.cfg.scene_fps.max(5) as f32;
            let has_desktop = self.we_content.is_some() || !self.we_parked.is_empty();
            if has_desktop {
                let mut bundles = self.drain_all_we_bundles();
                let mut restored = Vec::with_capacity(bundles.len());
                for b in bundles.drain(..) {
                    let paused = b.present.paused;
                    let rate = b.present.rate.clamp(0.05, 4.0);
                    let dt = if paused {
                        0.0
                    } else {
                        raw_dt.min(0.1) * rate
                    };
                    self.install_bundle(b);
                    self.sync_present_to_videos(!paused, rate);
                    self.sync_script_videos_pre_tick();
                    if !paused {
                        if let Some(we_runtime::WeContent::Scene { runtime }) =
                            self.we_content.as_mut()
                        {
                            runtime.set_cursor_uv(self.cursor_uv[0], self.cursor_uv[1]);
                            runtime.tick(dt);
                        }
                    }
                    self.apply_script_video_commands();
                    if !paused {
                        self.pump_we_layer_videos();
                        self.pump_we_video_wallpaper();
                    }
                    max_fps = max_fps.max(self.we_present_fps());
                    // Draw only outputs that this bundle owns.
                    let ids: Vec<_> = self.outputs.keys().cloned().collect();
                    for id in ids {
                        if self.we_targets_output(&id) {
                            let _ = self.draw_we_on(id);
                        }
                    }
                    if let Some(taken) = self.take_active_bundle() {
                        restored.push(taken);
                    }
                }
                self.restore_we_bundles(restored);
            }

            // Editor GPU preview: swap the editor slot into the active we_*
            // fields for one offscreen capture, then put the desktop back.
            if self.editor_slot.is_some() {
                let desktop = self.take_active_bundle();
                let editor = self.editor_slot.take().unwrap();
                let paused = editor.present.paused;
                let rate = editor.present.rate.clamp(0.05, 4.0);
                let dt = if paused {
                    0.0
                } else {
                    raw_dt.min(0.1) * rate
                };
                self.install_bundle(editor);
                self.sync_present_to_videos(!paused, rate);
                self.sync_script_videos_pre_tick();
                if !paused {
                    if let Some(we_runtime::WeContent::Scene { runtime }) = self.we_content.as_mut()
                    {
                        runtime.set_cursor_uv(self.cursor_uv[0], self.cursor_uv[1]);
                        runtime.tick(dt);
                    }
                }
                self.apply_script_video_commands();
                if !paused {
                    self.pump_we_layer_videos();
                    self.pump_we_video_wallpaper();
                }
                max_fps = max_fps.max(self.we_present_fps());
                let _ = self.capture_editor_frame();
                self.editor_slot = self.take_active_bundle();
                if let Some(d) = desktop {
                    self.install_bundle(d);
                }
            }

            let frame = Duration::from_secs_f32(1.0 / max_fps.clamp(5.0, 120.0));
            next = Some(Instant::now() + frame);
        }

        // In-flight WE wipes redraw every tick even when both scenes are
        // otherwise static (they draw over the already-rendered live frame).
        let we_wipe_ids: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| o.wipe_we.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in we_wipe_ids {
            let _ = self.draw_we_on(id);
            let t = Instant::now() + Duration::from_millis(8);
            next = Some(next.map(|n| n.min(t)).unwrap_or(t));
        }

        // Scene particle tick + redraw all outputs that have surfaces.
        if self.scene_needs_anim() {
            let dt = self.last_scene_tick.elapsed().as_secs_f32();
            self.last_scene_tick = Instant::now();
            if let Some(sc) = self.scene.as_mut() {
                sc.tick(dt);
            }
            let ids: Vec<_> = self.outputs.keys().cloned().collect();
            for id in ids {
                let _ = self.draw_output(id);
            }
            let frame = Duration::from_secs_f32(1.0 / self.cfg.scene_fps.max(5) as f32);
            next = Some(Instant::now() + frame);
        }

        let ids: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| o.wipe.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if self.draw_output(id) {
                let t = Instant::now() + Duration::from_millis(8);
                next = Some(match next {
                    Some(n) => n.min(t),
                    None => t,
                });
            }
        }
        next
    }

    /// IPC dispatch. Writes the reply back on the socket.
    fn handle_ipc(&mut self, cmd: IpcCmd, mut stream: UnixStream) {
        let reply = match cmd {
            IpcCmd::Ping => "ok pong".to_string(),
            IpcCmd::Status => {
                let mut parts = vec!["ok".to_string()];
                let we_n = self.we_slot_count();
                if we_n > 0 {
                    parts.push(format!("we_slots={we_n}"));
                }
                if let Some(we) = &self.we_content {
                    parts.push(format!(
                        "{}={} (active)",
                        we.kind_tag(),
                        we.title().replace(" ", "_")
                    ));
                }
                if let Some(sc) = &self.scene {
                    let anim = if sc.is_animated() { "animated" } else { "static" };
                    parts.push(format!("scene={} ({anim})", sc.name()));
                }
                let mut outs: Vec<_> = self.outputs.values().collect();
                outs.sort_by(|a, b| a.info.name.cmp(&b.info.name));
                for o in outs {
                    let we_here = self.we_owns_monitor(&o.info.name);
                    let state = if o.surface.is_none() {
                        "stopped"
                    } else if we_here {
                        "we"
                    } else if self.scene.is_some() {
                        "scene"
                    } else if o.wipe.is_some() {
                        "wiping"
                    } else if o.pending_path.is_some() {
                        "loading"
                    } else if o.current.is_some() {
                        "shown"
                    } else {
                        "empty"
                    };
                    // Prefer path from the WE bundle bound to this monitor.
                    let path = self
                        .we_dir_for_monitor(&o.info.name)
                        .map(|p| p.display().to_string())
                        .or_else(|| {
                            o.current_path
                                .as_deref()
                                .map(|p| p.display().to_string())
                        })
                        .unwrap_or_default();
                    parts.push(format!("{}={path} ({state})", o.info.name));
                }
                parts.join(" ")
            }
            IpcCmd::Reload => {
                self.reload(None);
                "ok reloaded".to_string()
            }
            IpcCmd::Set { monitor, path } => {
                self.set_wallpaper(&monitor, &path, None);
                self.maybe_accent_refresh();
                "ok".to_string()
            }
            IpcCmd::Snap { monitor, path } => {
                self.set_wallpaper(&monitor, &path, Some(Transition::Snap));
                self.maybe_accent_refresh();
                "ok".to_string()
            }
            IpcCmd::Wipe { monitor, path } => {
                self.set_wallpaper(&monitor, &path, Some(Transition::Wipe));
                self.maybe_accent_refresh();
                "ok".to_string()
            }
            IpcCmd::Preload { path } => {
                if self.cache.iter().any(|(p, _)| *p == path) {
                    "ok cached".to_string()
                } else if self.in_flight.insert(path.clone()) {
                    let _ = self.decode_tx.send(path);
                    "ok queued".to_string()
                } else {
                    "ok in-flight".to_string()
                }
            }
            IpcCmd::Stop => {
                // Hide everything (video handoff): destroy layer surfaces, free textures.
                let ids: Vec<_> = self.outputs.keys().cloned().collect();
                for id in ids {
                    self.destroy_surface(id);
                }
                log::info!("stop: all surfaces unmapped (video handoff)");
                "ok stopped".to_string()
            }
            IpcCmd::Start => {
                let ids: Vec<_> = self.outputs.keys().cloned().collect();
                for id in ids {
                    self.ensure_surface(id);
                }
                "ok started".to_string()
            }
            IpcCmd::Ready => {
                // "ready" = every known output has a visible frame (or no outputs yet).
                let has_content = self.scene.is_some() || self.we_content.is_some();
                let waiting = self
                    .outputs
                    .values()
                    .filter(|o| {
                        !o.surface.as_ref().is_some_and(|s| s.committed)
                            || (!has_content && o.current.is_none() && o.wipe.is_none())
                    })
                    .count();
                if waiting == 0 {
                    "ok ready".to_string()
                } else {
                    format!("err waiting:{waiting}")
                }
            }
            IpcCmd::Scene { monitor, path } => {
                let _ = monitor; // native scenes still paint all outputs for now
                self.clear_we();
                match self.load_scene(&path) {
                    Ok(()) => {
                        self.maybe_accent_refresh();
                        "ok scene".to_string()
                    }
                    Err(e) => format!("err {e}"),
                }
            }
            IpcCmd::We { monitor, path } => {
                let res = match self.load_we_package_on(&path, &monitor) {
                    Ok(()) => format!(
                        "ok we {} mon={}",
                        self.we_content
                            .as_ref()
                            .map(|c| c.title())
                            .unwrap_or("loaded"),
                        if self.we_monitors.is_empty()
                            || self.we_monitors.iter().any(|m| m == "*")
                        {
                            "*".into()
                        } else {
                            self.we_monitors.join(",")
                        }
                    ),
                    Err(e) => format!("err {e}"),
                };
                if res.starts_with("ok") {
                    self.maybe_accent_refresh();
                }
                res
            }
            IpcCmd::WeStop => {
                self.clear_we();
                session::clear();
                // restore classic config walls
                self.reload(Some(Transition::Snap));
                self.maybe_accent_refresh();
                "ok we_stop".to_string()
            }
            IpcCmd::WeProps { path } => match self.handle_we_props(&path) {
                Ok(s) => s,
                Err(e) => format!("err {e}"),
            },
            IpcCmd::WeSetProp { path, key, value } => {
                match self.handle_we_set_prop(&path, &key, &value) {
                    Ok(s) => s,
                    Err(e) => format!("err {e}"),
                }
            }
            IpcCmd::WeResetProps { path } => match self.handle_we_reset_props(&path) {
                Ok(s) => s,
                Err(e) => format!("err {e}"),
            },
            IpcCmd::WePresent { args } => self.handle_we_present(&args),
            IpcCmd::WeEditor { args } => self.handle_we_editor(&args),
            IpcCmd::WeDebug { args } => self.handle_we_debug(&args),
            IpcCmd::CfgReload => {
                // Options only: wallstudio changes fps / quality while a
                // wallpaper is live, and a full reload would tear it down.
                let old = self.cfg.clone();
                let fresh = config::load_global();
                self.cfg.scene_fps = fresh.scene_fps;
                self.cfg.video_max_edge = fresh.video_max_edge;
                self.cfg.transition = fresh.transition;
                self.cfg.wipe_ms = fresh.wipe_ms;
                self.cfg.wipe_feather_px = fresh.wipe_feather_px;
                let cap = if self.cfg.video_max_edge == 0 {
                    "native".to_string()
                } else {
                    format!("{}px", self.cfg.video_max_edge)
                };
                if old.video_max_edge != self.cfg.video_max_edge {
                    log::info!(
                        "cfg_reload: video_max_edge {} → {} (applies to the next wallpaper load)",
                        old.video_max_edge,
                        self.cfg.video_max_edge
                    );
                }
                format!("ok cfg scene_fps={} video_max_edge={cap}", self.cfg.scene_fps)
            }
            IpcCmd::BootCapture => {
                self.arm_boot_capture();
                // User-forced: skip the boot grace and write on next present.
                self.boot_capture_after = Some(Instant::now());
                for name in self.boot_capture_pending.iter().cloned().collect::<Vec<_>>() {
                    self.boot_capture_frames.insert(name, 10);
                }
                if self.boot_capture_pending.is_empty() {
                    for o in self.outputs.values() {
                        if !o.info.name.is_empty() {
                            self.boot_capture_pending.insert(o.info.name.clone());
                            self.boot_capture_frames.insert(o.info.name.clone(), 10);
                        }
                    }
                }
                "ok boot_capture armed".into()
            }
            IpcCmd::Quit => {
                log::info!("quit requested via IPC");
                self.running = false;
                if let Some(sig) = &self.loop_signal {
                    sig.stop();
                }
                "ok bye".to_string()
            }
        };
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.write_all(b"\n");
    }

    /// Debug IPC: click correlation, layer dump, clear markers.
    ///
    /// ```text
    /// walld ctl we_debug click 0.72 0.18     # normalized UV (0..1), y-down
    /// walld ctl we_debug click 1850 256      # raw viewport pixels
    /// walld ctl we_debug dump
    /// walld ctl we_debug clear
    /// ```
    fn handle_we_debug(&mut self, args: &[String]) -> String {
        let Some(sub) = args.first().map(|s| s.as_str()) else {
            return "err usage: we_debug click|dump|clear …".into();
        };
        match sub {
            "clear" => {
                if let Some(we_runtime::WeContent::Scene { runtime }) = self.we_content.as_mut() {
                    runtime.debug_clicks.clear();
                }
                let _ = std::fs::remove_file("/tmp/walld_debug.json");
                "ok cleared".into()
            }
            "dump" => {
                let Some(we_runtime::WeContent::Scene { runtime }) = &self.we_content else {
                    return "err no WE scene loaded".into();
                };
                let ow = runtime.ortho_width;
                let oh = runtime.ortho_height;
                let mut lines = vec![format!(
                    "scene «{}» ortho={ow}x{oh} images={} particles={} clicks={}",
                    runtime.title,
                    runtime.images.len(),
                    runtime.particles.len(),
                    runtime.debug_clicks.len()
                )];
                for (i, img) in runtime.images.iter().enumerate() {
                    let [sx, sy] =
                        wallengine_we::camera_to_screen(img.origin[0], img.origin[1], ow, oh);
                    lines.push(format!(
                        "  img[{i}] «{}» origin_s=({sx:.0},{sy:.0}) cam=({:.0},{:.0}) size=({:.0}x{:.0}) puppet={} order={}",
                        img.name,
                        img.origin[0],
                        img.origin[1],
                        img.size[0] * img.scale[0].abs(),
                        img.size[1] * img.scale[1].abs(),
                        img.puppet.is_some(),
                        img.scene_order
                    ));
                }
                for (i, p) in runtime.particles.iter().enumerate() {
                    let [sx, sy] =
                        wallengine_we::camera_to_screen(p.origin_cam[0], p.origin_cam[1], ow, oh);
                    lines.push(format!(
                        "  part[{i}] «{}» origin_s=({sx:.0},{sy:.0}) alive={} alpha_mul={:.2}",
                        p.name,
                        p.alive().count(),
                        p.alpha_mul
                    ));
                }
                for (i, c) in runtime.debug_clicks.iter().enumerate() {
                    lines.push(format!(
                        "  click[{i}] view=({:.0},{:.0}) ortho=({:.1},{:.1}) cam=({:.1},{:.1}) uv=({:.3},{:.3})",
                        c.view_x,
                        c.view_y,
                        c.ortho_x,
                        c.ortho_y,
                        c.cam_x,
                        c.cam_y,
                        c.ortho_x / ow.max(1.0),
                        c.ortho_y / oh.max(1.0)
                    ));
                }
                // Also write machine-readable JSON for the agent / tools.
                let json = serde_json::json!({
                    "ortho": [ow, oh],
                    "clicks": runtime.debug_clicks.iter().map(|c| serde_json::json!({
                        "view": [c.view_x, c.view_y, c.view_w, c.view_h],
                        "ortho": [c.ortho_x, c.ortho_y],
                        "cam": [c.cam_x, c.cam_y],
                        "uv": [c.ortho_x / ow.max(1.0), c.ortho_y / oh.max(1.0)],
                    })).collect::<Vec<_>>(),
                    "images": runtime.images.iter().map(|img| {
                        let [sx, sy] = wallengine_we::camera_to_screen(img.origin[0], img.origin[1], ow, oh);
                        serde_json::json!({
                            "name": img.name,
                            "origin_screen": [sx, sy],
                            "origin_cam": [img.origin[0], img.origin[1]],
                            "size": [img.size[0]*img.scale[0].abs(), img.size[1]*img.scale[1].abs()],
                            "puppet": img.puppet.is_some(),
                        })
                    }).collect::<Vec<_>>(),
                });
                let _ = std::fs::write(
                    "/tmp/walld_debug.json",
                    serde_json::to_string_pretty(&json).unwrap_or_default(),
                );
                format!("ok {}", lines.join(" | "))
            }
            "click" => {
                let x: f32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(-1.0);
                let y: f32 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(-1.0);
                if x < 0.0 || y < 0.0 {
                    return "err usage: we_debug click <x> <y>".into();
                }
                // Prefer first output size; fall back to ortho.
                let (vw, vh) = self
                    .outputs
                    .values()
                    .find_map(|o| {
                        o.surface
                            .as_ref()
                            .filter(|s| s.width > 0 && s.height > 0)
                            .map(|s| (s.width as f32, s.height as f32))
                    })
                    .unwrap_or_else(|| {
                        if let Some(we_runtime::WeContent::Scene { runtime }) = &self.we_content {
                            (runtime.ortho_width, runtime.ortho_height)
                        } else {
                            (1920.0, 1080.0)
                        }
                    });
                // Accept normalized 0..1 or absolute pixels.
                let (px, py) = if x <= 1.5 && y <= 1.5 {
                    (x * vw, y * vh)
                } else {
                    (x, y)
                };
                let reply = {
                    let Some(we_runtime::WeContent::Scene { runtime }) = self.we_content.as_mut()
                    else {
                        return "err no WE scene loaded".into();
                    };
                    runtime.push_debug_click(px, py, vw, vh);
                    if let Some(c) = runtime.debug_clicks.last().copied() {
                        format!(
                            "ok click view=({:.0},{:.0}) ortho=({:.1},{:.1}) cam=({:.1},{:.1}) uv=({:.4},{:.4}) → dump writes /tmp/walld_debug.json",
                            c.view_x,
                            c.view_y,
                            c.ortho_x,
                            c.ortho_y,
                            c.cam_x,
                            c.cam_y,
                            c.ortho_x / runtime.ortho_width.max(1.0),
                            c.ortho_y / runtime.ortho_height.max(1.0)
                        )
                    } else {
                        "ok click".into()
                    }
                };
                // redraw so marker appears (after runtime borrow ends)
                let ids: Vec<_> = self.outputs.keys().cloned().collect();
                for id in ids {
                    self.draw_output(id);
                }
                reply
            }
            other => format!("err unknown we_debug subcommand '{other}'"),
        }
    }

    /// Tear down an output's layer surface + textures (stop / output removal).
    fn destroy_surface(&mut self, out_id: wayland_client::backend::ObjectId) {
        let Some(out) = self.outputs.get_mut(&out_id) else { return };
        if let Some(s) = out.surface.take() {
            // EGL window must be destroyed before the wl_surface backing it.
            drop(s.egl_window);
            s.layer.destroy();
            s.wl_surface.destroy();
        }
        if let Some(wipe) = out.wipe.take() {
            self.renderer.delete_texture(wipe.old.tex);
            self.renderer.delete_texture(wipe.new.tex);
        }
        if let Some(w) = out.wipe_we.take() {
            self.renderer.delete_texture(w.tex);
        }
        out.wipe_pending_first = false;
        if let Some(cur) = out.current.take() {
            self.renderer.delete_texture(cur.tex);
        }
        out.current_path = None;
        out.pending_path = None;
    }

    fn map_fit(f: SceneFit) -> FitMode {
        match f {
            SceneFit::Cover => FitMode::Cover,
            SceneFit::Contain => FitMode::Contain,
            SceneFit::Fill => FitMode::Fill,
        }
    }

    /// Load a scene file and warm image textures. Applies to all outputs for now.
    fn load_scene(&mut self, path: &std::path::Path) -> Result<(), String> {
        let rt = SceneRuntime::load(path)?;
        log::info!("scene loaded: {} ({})", rt.name(), path.display());
        // Upload image layers (sync decode for prototype).
        let paths: Vec<PathBuf> = rt
            .image_paths
            .iter()
            .filter_map(|p| p.clone())
            .collect();
        for p in paths {
            if self.scene_tex.contains_key(&p) {
                continue;
            }
            match image::decode_file(&p) {
                Ok(img) => {
                    let tex = self.renderer.upload_rgba(&img.rgba, img.width, img.height);
                    self.scene_tex.insert(
                        p.clone(),
                        Texture {
                            tex,
                            w: img.width,
                            h: img.height,
                            uv_scale: (1.0, 1.0),
                        },
                    );
                    log::debug!("scene tex {} ({}x{})", p.display(), img.width, img.height);
                }
                Err(e) => log::warn!("scene image {}: {e}", p.display()),
            }
        }
        self.scene = Some(rt);
        self.scene_path = Some(path.to_path_buf());
        // Clear classic wipe/current so scene owns the frame.
        let ids: Vec<_> = self.outputs.keys().cloned().collect();
        for id in ids {
            if let Some(out) = self.outputs.get_mut(&id) {
                if let Some(w) = out.wipe.take() {
                    self.renderer.delete_texture(w.old.tex);
                    self.renderer.delete_texture(w.new.tex);
                }
                if let Some(w) = out.wipe_we.take() {
                    self.renderer.delete_texture(w.tex);
                }
                out.wipe_pending_first = false;
                // keep current textures alive? free them — scene uses scene_tex
                if let Some(c) = out.current.take() {
                    self.renderer.delete_texture(c.tex);
                }
                out.current_path = self.scene_path.clone();
                out.pending_path = None;
            }
            self.draw_output(id);
        }
        self.last_scene_tick = Instant::now();
        Ok(())
    }

    fn clear_scene(&mut self) {
        self.scene = None;
        self.scene_path = None;
        // Free scene textures
        let texs: Vec<_> = self.scene_tex.drain().map(|(_, t)| t).collect();
        for t in texs {
            self.renderer.delete_texture(t.tex);
        }
    }

    fn scene_needs_anim(&self) -> bool {
        self.scene.as_ref().is_some_and(|s| s.is_animated())
    }

    fn draw_scene_on(&mut self, out_id: wayland_client::backend::ObjectId) -> bool {
        let Some(out) = self.outputs.get(&out_id) else { return false };
        let Some(s) = out.surface.as_ref() else { return false };
        let Some(win) = s.egl_window.as_ref() else { return false };
        if s.width == 0 || s.height == 0 {
            return false;
        }
        let (w, h) = (s.width as i32, s.height as i32);
        if self.renderer.attach_window(win).is_err() {
            return false;
        }

        // Borrow scene data without holding &mut outputs across draws.
        let clear = self
            .scene
            .as_ref()
            .map(|sc| sc.doc.clear)
            .unwrap_or([0.0, 0.0, 0.0, 1.0]);
        self.renderer.begin_frame(w, h, clear);

        // Collect draw ops first to avoid borrow issues.
        enum Op {
            Color([f32; 4], f32),
            Image(PathBuf, FitMode, f32),
            Particles(usize, f32), // index into scene.particles
        }
        let mut ops = Vec::new();
        if let Some(sc) = self.scene.as_ref() {
            for (i, layer) in sc.doc.layers.iter().enumerate() {
                match layer {
                    wallengine_scene::LayerDoc::Color { color, opacity, .. } => {
                        ops.push(Op::Color(*color, *opacity));
                    }
                    wallengine_scene::LayerDoc::Image { fit, opacity, .. } => {
                        if let Some(path) = sc.image_paths[i].clone() {
                            ops.push(Op::Image(path, Self::map_fit(*fit), *opacity));
                        }
                    }
                    wallengine_scene::LayerDoc::Particles { opacity, .. } => {
                        ops.push(Op::Particles(i, *opacity));
                    }
                }
            }
        }

        for op in ops {
            match op {
                Op::Color(color, opacity) => {
                    self.renderer.draw_color_layer(w, h, color, opacity);
                }
                Op::Image(path, fit, opacity) => {
                    if let Some(tex) = self.scene_tex.get(&path) {
                        self.renderer
                            .draw_blit_layer(w, h, tex.tex, tex.w, tex.h, fit, opacity);
                    }
                }
                Op::Particles(i, opacity) => {
                    if let Some(sc) = self.scene.as_ref() {
                        if let Some(sys) = sc.particles[i].as_ref() {
                            self.renderer
                                .draw_particles(w, h, &sys.particles, opacity * sys.opacity);
                        }
                    }
                }
            }
        }

        self.renderer.swap();
        if let Some(out) = self.outputs.get_mut(&out_id) {
            if let Some(s) = out.surface.as_mut() {
                s.wl_surface.commit();
                s.committed = true;
            }
        }
        self.scene_needs_anim()
    }

    /// Run the same effect executor used by headless pixel regression tests.
    fn run_layer_effects(
        &mut self,
        layer_index: usize,
        albedo: Texture,
        time: f32,
    ) -> Option<effects::EffectOutput> {
        let Some(we_runtime::WeContent::Scene { runtime }) = &self.we_content else {
            return None;
        };
        let layer = runtime.images.get(layer_index)?;
        let programs = self.we_fx_progs.get(&layer_index)?;
        Some(self.we_fx_targets.run(
            &self.renderer,
            effects::EffectTexture { tex: albedo.tex, w: albedo.w, h: albedo.h, uv_scale: albedo.uv_scale },
            &layer.effect_passes, programs, &runtime.graph, time, self.cursor_uv,
            |name| {
                let texture = self.we_fx_tex.get(name).copied().or_else(|| {
                    let rest = name.strip_prefix("_rt_imageLayerComposite_")?;
                    let id: i64 = rest.split('_').next()?.parse().ok()?;
                    let idx = runtime.images.iter().position(|l| l.node_id == id)?;
                    self.we_layer_tex.get(idx).copied().flatten()
                })?;
                Some(effects::EffectTexture { tex: texture.tex, w: texture.w, h: texture.h, uv_scale: texture.uv_scale })
            },
        ))
    }

    /// Longest edge among connected outputs (pixels). Used so video textures
    /// decode at least as large as the biggest monitor — not a fixed 1080p cap.
    fn desktop_max_edge(&self) -> u32 {
        let mut edge = 0u32;
        for o in self.outputs.values() {
            let (w, h) = if let Some(s) = o.surface.as_ref() {
                (s.width, s.height)
            } else if o.info.width > 0 && o.info.height > 0 {
                (o.info.width as u32, o.info.height as u32)
            } else {
                continue;
            };
            edge = edge.max(w).max(h);
        }
        // Sensible default before outputs are ready; still above 1080p long-edge.
        let edge = edge.max(2560).min(7680);
        // An explicit cap only ever lowers the resolution.
        if self.cfg.video_max_edge > 0 {
            return edge.min(self.cfg.video_max_edge);
        }
        edge
    }

    fn handle_we_props(&self, path: &Path) -> Result<String, String> {
        let dir = Self::resolve_we_dir(path)?;
        let id = Self::workshop_id_for_dir(&dir);
        let props = wallengine_we::list_props(&dir, &id);
        let list: Vec<serde_json::Value> = props
            .iter()
            .map(|p| {
                let mut o = serde_json::Map::new();
                o.insert("key".into(), serde_json::json!(p.key));
                o.insert("label".into(), serde_json::json!(p.label));
                o.insert("order".into(), serde_json::json!(p.order));
                match &p.kind {
                    wallengine_we::PropKind::Bool => {
                        o.insert("type".into(), serde_json::json!("bool"));
                    }
                    wallengine_we::PropKind::Slider {
                        min,
                        max,
                        step,
                        fraction,
                    } => {
                        o.insert("type".into(), serde_json::json!("slider"));
                        o.insert("min".into(), serde_json::json!(min));
                        o.insert("max".into(), serde_json::json!(max));
                        o.insert("step".into(), serde_json::json!(step));
                        o.insert("fraction".into(), serde_json::json!(fraction));
                    }
                    wallengine_we::PropKind::Color => {
                        o.insert("type".into(), serde_json::json!("color"));
                    }
                    wallengine_we::PropKind::Text => {
                        o.insert("type".into(), serde_json::json!("text"));
                    }
                    wallengine_we::PropKind::Combo { options } => {
                        o.insert("type".into(), serde_json::json!("combo"));
                        o.insert(
                            "options".into(),
                            serde_json::json!(options
                                .iter()
                                .map(|(l, v)| serde_json::json!({"label": l, "value": v}))
                                .collect::<Vec<_>>()),
                        );
                    }
                    wallengine_we::PropKind::Group => {
                        o.insert("type".into(), serde_json::json!("group"));
                    }
                    wallengine_we::PropKind::Other(t) => {
                        o.insert("type".into(), serde_json::json!(t));
                    }
                }
                o.insert("value".into(), p.value.as_json());
                serde_json::Value::Object(o)
            })
            .collect();
        Ok(format!(
            "ok {}",
            serde_json::to_string(&list).unwrap_or_else(|_| "[]".into())
        ))
    }

    fn handle_we_set_prop(&mut self, path: &Path, key: &str, value: &str) -> Result<String, String> {
        let dir = Self::resolve_we_dir(path)?;
        let id = Self::workshop_id_for_dir(&dir);
        let raw = wallengine_we::load_raw_properties(&dir);
        let spec = raw.get(key).cloned().unwrap_or(serde_json::json!({
            "type": "text",
            "value": value,
        }));
        let json_val = wallengine_we::parse_value_for_prop(&spec, value)?;
        wallengine_we::set_override(&id, key, json_val.clone())?;
        // Hot-reload every live instance of this package (each keeps its monitors).
        let n = self.reload_matching_we(&dir, &id)?;
        Ok(format!(
            "ok set {key}={}{}",
            json_val,
            if n > 0 {
                format!(" (reloaded {n})")
            } else {
                String::new()
            }
        ))
    }

    fn handle_we_reset_props(&mut self, path: &Path) -> Result<String, String> {
        let dir = Self::resolve_we_dir(path)?;
        let id = Self::workshop_id_for_dir(&dir);
        wallengine_we::clear_overrides(&id)?;
        let n = self.reload_matching_we(&dir, &id)?;
        Ok(format!(
            "ok reset props for {id}{}",
            if n > 0 {
                format!(" (reloaded {n})")
            } else {
                String::new()
            }
        ))
    }

    /// Reload every live WE slot whose package matches `dir`/`id`, preserving
    /// each slot's monitor list so a prop edit never spills across displays.
    fn reload_matching_we(&mut self, dir: &Path, id: &str) -> Result<usize, String> {
        let mut bundles = self.drain_all_we_bundles();
        let mut kept = Vec::new();
        let mut reload_mons = Vec::new();
        for b in bundles.drain(..) {
            if b.dir == dir || Self::workshop_id_for_dir(&b.dir) == id {
                let mon = if bundle_monitors_is_all(&b.monitors) {
                    "*".to_string()
                } else {
                    b.monitors.join(",")
                };
                reload_mons.push(mon);
                self.free_bundle(b);
            } else {
                kept.push(b);
            }
        }
        self.restore_we_bundles(kept);
        let n = reload_mons.len();
        for mon in reload_mons {
            self.load_we_package_on(dir, &mon)?;
        }
        Ok(n)
    }

    /// True when the *active* WE slot paints every output.
    fn we_monitors_is_all(&self) -> bool {
        bundle_monitors_is_all(&self.we_monitors)
    }

    /// Whether `monitor_name` is in the *active* WE target list.
    fn we_monitor_name_matches(&self, monitor_name: &str) -> bool {
        bundle_targets_name(&self.we_monitors, monitor_name)
    }

    fn we_targets_output(&self, out_id: &wayland_client::backend::ObjectId) -> bool {
        let Some(name) = self.outputs.get(out_id).map(|o| o.info.name.as_str()) else {
            return false;
        };
        if name.is_empty() {
            return false;
        }
        self.we_monitor_name_matches(name)
    }

    fn we_slot_count(&self) -> usize {
        self.we_parked.len() + usize::from(self.we_content.is_some())
    }

    /// Whether any live WE slot owns this monitor name.
    fn we_owns_monitor(&self, name: &str) -> bool {
        self.we_dir_for_monitor(name).is_some()
    }

    /// Pull the active `we_*` fields into a parked-able bundle (does not free GPU).
    fn take_active_bundle(&mut self) -> Option<WeBundle> {
        let content = self.we_content.take()?;
        let dir = self.we_dir.take().unwrap_or_else(|| PathBuf::from("."));
        Some(WeBundle {
            content,
            dir,
            monitors: std::mem::take(&mut self.we_monitors),
            present: std::mem::take(&mut self.we_present),
            present_mon: std::mem::take(&mut self.we_present_mon),
            started: self.we_started,
            layer_tex: std::mem::take(&mut self.we_layer_tex),
            layer_video: std::mem::take(&mut self.we_layer_video),
            text_tex: std::mem::take(&mut self.we_text_tex),
            particle_tex: std::mem::take(&mut self.we_particle_tex),
            fx_progs: std::mem::take(&mut self.we_fx_progs),
            fx_tex: std::mem::take(&mut self.we_fx_tex),
            mask_tex: std::mem::take(&mut self.we_mask_tex),
            phase_tex: std::mem::take(&mut self.we_phase_tex),
        })
    }

    fn install_bundle(&mut self, b: WeBundle) {
        debug_assert!(self.we_content.is_none(), "install_bundle over active WE");
        self.we_content = Some(b.content);
        self.we_dir = Some(b.dir);
        self.we_monitors = b.monitors;
        self.we_present = b.present;
        self.we_present_mon = b.present_mon;
        self.we_started = b.started;
        self.we_layer_tex = b.layer_tex;
        self.we_layer_video = b.layer_video;
        self.we_text_tex = b.text_tex;
        self.we_particle_tex = b.particle_tex;
        self.we_fx_progs = b.fx_progs;
        self.we_fx_tex = b.fx_tex;
        self.we_mask_tex = b.mask_tex;
        self.we_phase_tex = b.phase_tex;
    }

    /// Effective present for drawing on `monitor` (base + visual override).
    fn effective_present_for(&self, monitor: &str) -> present::WePresent {
        self.we_present
            .with_monitor_visual(self.we_present_mon.get(monitor))
    }

    fn drain_all_we_bundles(&mut self) -> Vec<WeBundle> {
        let mut out = Vec::new();
        if let Some(b) = self.take_active_bundle() {
            out.push(b);
        }
        out.append(&mut self.we_parked);
        out
    }

    fn restore_we_bundles(&mut self, mut bundles: Vec<WeBundle>) {
        self.we_parked.clear();
        if bundles.is_empty() {
            return;
        }
        // Keep the first as active for status / prop convenience; rest parked.
        let first = bundles.remove(0);
        self.install_bundle(first);
        self.we_parked = bundles;
    }

    /// Free GPU resources owned by one bundle (not shared fx ping-pong targets).
    fn free_bundle(&mut self, mut b: WeBundle) {
        let n = b.layer_video.len();
        for i in 0..n {
            let is_mpv = b.layer_video[i]
                .as_ref()
                .map(|d| d.is_mpv())
                .unwrap_or(false);
            if is_mpv {
                if let Some(d) = b.layer_video[i].as_mut() {
                    d.destroy_mpv_gl(&self.renderer.gl);
                }
                if let Some(slot) = b.layer_tex.get_mut(i) {
                    *slot = None;
                }
            }
        }
        if let we_runtime::WeContent::Video { decoder, .. } = &mut b.content {
            if decoder.is_mpv() {
                decoder.destroy_mpv_gl(&self.renderer.gl);
                if let Some(slot) = b.layer_tex.first_mut() {
                    *slot = None;
                }
            }
        }
        b.layer_video.clear();
        for t in b.layer_tex.drain(..) {
            if let Some(tex) = t {
                self.renderer.delete_texture(tex.tex);
            }
        }
        for t in b.text_tex.drain(..) {
            if let Some((tex, _)) = t {
                self.renderer.delete_texture(tex.tex);
            }
        }
        for t in b.particle_tex.drain(..) {
            if let Some(tex) = t {
                self.renderer.delete_texture(tex.tex);
            }
        }
        for (_, tex) in b.fx_tex.drain() {
            self.renderer.delete_texture(tex.tex);
        }
        b.fx_progs.clear();
        for t in b.mask_tex.drain(..) {
            if let Some(tex) = t {
                self.renderer.delete_texture(tex.tex);
            }
        }
        for t in b.phase_tex.drain(..) {
            if let Some(tex) = t {
                self.renderer.delete_texture(tex.tex);
            }
        }
        drop(b.content);
    }

    /// Drop classic-image claim: remove `monitor` from every WE bundle.
    fn detach_we_monitor(&mut self, monitor: &str) {
        let mut bundles = self.drain_all_we_bundles();
        let all_names: Vec<String> = self
            .outputs
            .values()
            .map(|o| o.info.name.clone())
            .filter(|n| !n.is_empty())
            .collect();
        let mut kept = Vec::new();
        for mut b in bundles.drain(..) {
            if bundle_monitors_is_all(&b.monitors) {
                b.monitors = all_names
                    .iter()
                    .filter(|n| n.as_str() != monitor)
                    .cloned()
                    .collect();
            } else {
                b.monitors
                    .retain(|m| m != monitor && !m.eq_ignore_ascii_case(monitor));
            }
            if b.monitors.is_empty() {
                self.free_bundle(b);
            } else {
                kept.push(b);
            }
        }
        self.restore_we_bundles(kept);
    }

    /// Make sure the active `we_*` slot is the one that owns `out_id`.
    fn ensure_we_active_for_output(&mut self, out_id: &wayland_client::backend::ObjectId) -> bool {
        if self.we_content.is_some() && self.we_targets_output(out_id) {
            return true;
        }
        let name = match self.outputs.get(out_id) {
            Some(o) => o.info.name.clone(),
            None => return false,
        };
        if name.is_empty() {
            return false;
        }
        let Some(i) = self
            .we_parked
            .iter()
            .position(|b| bundle_targets_name(&b.monitors, &name))
        else {
            return false;
        };
        if let Some(cur) = self.take_active_bundle() {
            self.we_parked.push(cur);
        }
        let b = self.we_parked.remove(i);
        self.install_bundle(b);
        true
    }

    /// Whether any WE slot (desktop active/parked, or editor preview) needs the loop.
    fn any_we_needs_anim(&self) -> bool {
        if self.editor_slot.is_some() {
            // Always pump editor captures so the live preview advances even
            // when the edited scene is "static" (flow materials, etc.).
            return true;
        }
        if self.slot_needs_anim_active() {
            return true;
        }
        for b in &self.we_parked {
            if Self::bundle_needs_anim(b) {
                return true;
            }
        }
        false
    }

    fn bundle_needs_anim(b: &WeBundle) -> bool {
        if b.layer_video.iter().any(|d| d.is_some()) {
            return true;
        }
        match &b.content {
            we_runtime::WeContent::Video { .. } => true,
            we_runtime::WeContent::Scene { runtime } => runtime.is_animated(),
        }
    }

    fn slot_needs_anim_active(&self) -> bool {
        if self.we_layer_video.iter().any(|d| d.is_some()) {
            return true;
        }
        match &self.we_content {
            Some(we_runtime::WeContent::Video { .. }) => true,
            Some(we_runtime::WeContent::Scene { runtime }) => runtime.is_animated(),
            None => false,
        }
    }

    /// Path of the WE package on `monitor`, if any.
    fn we_dir_for_monitor(&self, name: &str) -> Option<PathBuf> {
        if self.we_content.is_some() && bundle_targets_name(&self.we_monitors, name) {
            return self.we_dir.clone();
        }
        self.we_parked
            .iter()
            .find(|b| bundle_targets_name(&b.monitors, name))
            .map(|b| b.dir.clone())
    }

    fn clear_we(&mut self) {
        let bundles = self.drain_all_we_bundles();
        for b in bundles {
            self.free_bundle(b);
        }
        if let Some(ed) = self.editor_slot.take() {
            self.free_bundle(ed);
        }
        // Shared size-keyed effect RTs / scene RT — only drop when nothing WE remains.
        self.we_fx_targets.clear(&self.renderer);
        if let Some((_, (f, t))) = self.we_scene_rt.take() {
            self.renderer.delete_target(f, t);
        }
        if let Some((_, (f, t))) = self.editor_fbo.take() {
            self.renderer.delete_target(f, t);
        }
        if let Some((_, (f, t))) = self.we_still_fbo.take() {
            self.renderer.delete_target(f, t);
        }
        // Any in-flight WE wipes point at content we just freed.
        for out in self.outputs.values_mut() {
            if let Some(w) = out.wipe_we.take() {
                self.renderer.delete_texture(w.tex);
            }
            out.wipe_pending_first = false;
        }
    }

    /// Free only the editor preview slot; desktop WE stays live.
    fn clear_editor_slot(&mut self) {
        if let Some(ed) = self.editor_slot.take() {
            self.free_bundle(ed);
        }
        self.editor_capture_active = false;
        if let Some((_, (f, t))) = self.editor_fbo.take() {
            self.renderer.delete_target(f, t);
        }
    }

    fn load_we_package(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.load_we_package_on(path, "*")
    }

    fn load_we_package_on(
        &mut self,
        path: &std::path::Path,
        monitor: &str,
    ) -> Result<(), String> {
        self.load_we_package_ex(path, false, monitor)
    }

    fn load_we_package_ex(
        &mut self,
        path: &std::path::Path,
        for_editor: bool,
        monitor: &str,
    ) -> Result<(), String> {
        let dir = Self::resolve_we_dir(path)?;
        // Outputs that previously painted WE (so we can restore classic wallpaper
        // when they are no longer in the target list).
        let prev_we_outs: Vec<wayland_client::backend::ObjectId> = self
            .outputs
            .keys()
            .filter(|id| {
                self.outputs
                    .get(id)
                    .map(|o| self.we_owns_monitor(&o.info.name))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();

        // Before anything is freed: still-capture the outgoing WE frame for every
        // output this package replaces on (FBO render — never window readback,
        // which freezes WE video on AMD/Mesa). Armed per output below as the
        // wipe's old side, blended over the live incoming scene in `draw_we_on`.
        let mut we_stills: Vec<(wayland_client::backend::ObjectId, glow::Texture)> = Vec::new();
        if !for_editor && self.cfg.transition == Transition::Wipe {
            let new_targets = parse_monitor_list(monitor);
            let still_ids: Vec<_> = self
                .outputs
                .keys()
                .filter(|id| {
                    prev_we_outs.contains(id)
                        && self
                            .outputs
                            .get(id)
                            .map(|o| {
                                !o.info.name.is_empty()
                                    && bundle_targets_name(&new_targets, &o.info.name)
                            })
                            .unwrap_or(false)
                })
                .cloned()
                .collect();
            for id in still_ids {
                if let Some(tex) = self.still_capture_we_for(&id) {
                    we_stills.push((id, tex));
                }
            }
        }

        // Editor: stash desktop bundles, load into the active we_* fields, then
        // move the result into `editor_slot` and put the desktop back. Never
        // park/stop live wallpaper just because the editor opened.
        let mut desktop_stash = Vec::new();
        if for_editor {
            desktop_stash = self.drain_all_we_bundles();
        } else {
            let targets = parse_monitor_list(monitor);
            let mut existing = self.drain_all_we_bundles();
            let mut preserved = Vec::new();
            if bundle_monitors_is_all(&targets) {
                // Explicit all-displays play replaces every slot.
                for b in existing.drain(..) {
                    self.free_bundle(b);
                }
            } else {
                let all_names: Vec<String> = self
                    .outputs
                    .values()
                    .map(|o| o.info.name.clone())
                    .filter(|n| !n.is_empty())
                    .collect();
                for mut b in existing.drain(..) {
                    if bundle_monitors_is_all(&b.monitors) {
                        // Expand "*" so we can peel off individual targets.
                        b.monitors = all_names.clone();
                    }
                    b.monitors.retain(|m| {
                        !targets
                            .iter()
                            .any(|t| t == m || t.eq_ignore_ascii_case(m))
                    });
                    if b.monitors.is_empty() {
                        self.free_bundle(b);
                    } else {
                        preserved.push(b);
                    }
                }
            }
            self.we_parked = preserved;
        }
        // leave classic scene mode
        if self.scene.is_some() {
            self.clear_scene();
        }
        // Texture uploads + effect compiles need a current GL context (IPC can
        // run long after the last draw). Keep the daemon pbuffer/window bound.
        let _ = self.renderer.make_current();
        let content = match we_runtime::load_we_dir(&dir) {
            Ok(c) => c,
            Err(e) => {
                // Don't lose other monitors' packages if this load fails.
                if for_editor && !desktop_stash.is_empty() {
                    self.restore_we_bundles(desktop_stash);
                }
                if let Some(t) = self.we_still_ready.take() {
                    self.renderer.delete_texture(t);
                }
                for (_, t) in we_stills.drain(..) {
                    self.renderer.delete_texture(t);
                }
                return Err(e);
            }
        };
        match &content {
            we_runtime::WeContent::Video { title, path, .. } => {
                log::info!("WE video «{title}» {}", path.display());
            }
            we_runtime::WeContent::Scene { runtime } => {
                log::info!(
                    "WE scene «{}» images={} particles={} animated={}",
                    runtime.title,
                    runtime.images.len(),
                    runtime.particles.len(),
                    runtime.is_animated()
                );
            }
        }
        // upload layer / mask / phase textures for scene
        self.we_layer_tex.clear();
        self.we_layer_video.clear();
        self.we_text_tex.clear();
        self.we_particle_tex.clear();
        self.we_mask_tex.clear();
        self.we_phase_tex.clear();
        if let we_runtime::WeContent::Scene { runtime } = &content {
            self.we_text_tex.resize_with(runtime.texts.len(), || None);
            for sys in &runtime.particles {
                self.we_particle_tex.push(sys.texture.as_ref().map(|t| {
                    let tex = self.renderer.upload_rgba(&t.rgba, t.width, t.height);
                    Texture {
                        tex,
                        w: t.width,
                        h: t.height,
                        uv_scale: t.content_uv_scale(),
                    }
                }));
            }
            // WE effect passes: compile shaders + upload their sampler textures.
            // Opt-in until the pass UV convention is right: g_TextureNResolution
            // must be (texW, texH, contentW, contentH); feeding (w,h,w,h) scales
            // UVs wrong and shrinks layers (seen on Christmas 2). WALLD_EFFECTS=1
            // enables the pipeline meanwhile.
            let fx_enabled = std::env::var("WALLD_EFFECTS")
                .map(|v| v != "0" && !v.eq_ignore_ascii_case("off"))
                .unwrap_or(true);
            let (mut ok, mut fail) = (0usize, 0usize);
            if fx_enabled {
            for (i, layer) in runtime.images.iter().enumerate() {
                let mut progs: Vec<Option<glow::Program>> = Vec::new();
                for eff in &layer.effect_passes {
                    for p in &eff.passes {
                        match self.renderer.compile_effect(&p.vert, &p.frag) {
                            Ok(pr) => {
                                progs.push(Some(pr));
                                ok += 1;
                            }
                            Err(e) => {
                                progs.push(None);
                                fail += 1;
                                log::debug!("effect {} compile failed: {e}", eff.file);
                            }
                        }
                        for name in p.textures.values() {
                            if self.we_fx_tex.contains_key(name) {
                                continue;
                            }
                            // VHS / static effects sample util/noise. Packages
                            // often omit it; synthesize a tiled noise map so TV
                            // censor static matches LWE instead of pure black.
                            let decoded = runtime.assets.load_tex(name).ok().or_else(|| {
                                let n = name.replace('\\', "/").to_ascii_lowercase();
                                if n.contains("noise") || n.ends_with("/noise") || n == "util/noise"
                                {
                                    Some(synth_noise_tex(256))
                                } else {
                                    None
                                }
                            });
                            if let Some(t) = decoded {
                                let tex =
                                    self.renderer.upload_rgba(&t.rgba, t.width, t.height);
                                self.we_fx_tex.insert(
                                    name.clone(),
                                    Texture {
                                        tex,
                                        w: t.width,
                                        h: t.height,
                                        uv_scale: t.content_uv_scale(),
                                    },
                                );
                            }
                        }
                    }
                }
                if progs.iter().any(|p| p.is_some()) {
                    self.we_fx_progs.insert(i, progs);
                }
            }
            }
            if ok + fail > 0 {
                log::info!("WE effect passes: {ok} compiled, {fail} unsupported");
            }
        }
        if let we_runtime::WeContent::Scene { runtime } = &content {
            // FFmpeg fallback publish cap; mpv ignores this and uses source timing.
            // Prefer ≥60 so dual-video 60fps scenes aren't halved.
            let display_fps = self.cfg.scene_fps.max(60);
            // Longest edge among active outputs (e.g. 2560 for 1440p). Never a
            // fixed 1080p cap — wallpapers should match the desktop.
            let max_edge = self.desktop_max_edge();
            for layer in &runtime.images {
                // No Y-flip: content is top-left in padded buffers; shaders use content_uv_scale.
                if let Some(ref t) = layer.rgba {
                    let tex = self.renderer.upload_rgba(&t.rgba, t.width, t.height);
                    let uv = t.content_uv_scale();
                    self.we_layer_tex.push(Some(Texture {
                        tex,
                        w: t.width,
                        h: t.height,
                        uv_scale: uv,
                    }));
                    // Stream embedded MP4s (front_sync / xray_sync style scenes).
                    let dec = t.video_path.as_ref().and_then(|p| {
                        match video::VideoDecoder::start_for_display(p, display_fps, max_edge) {
                            Ok(d) => {
                                log::info!(
                                    "WE video texture «{}» {}×{} @ {:.3}fps → {}×{} ({})",
                                    layer.name,
                                    d.info.width,
                                    d.info.height,
                                    d.info.fps,
                                    d.out_w,
                                    d.out_h,
                                    d.backend.as_str()
                                );
                                Some(d)
                            }
                            Err(e) => {
                                log::warn!(
                                    "WE video texture «{}» failed: {e}",
                                    layer.name
                                );
                                None
                            }
                        }
                    });
                    self.we_layer_video.push(dec);
                } else {
                    self.we_layer_tex.push(None);
                    self.we_layer_video.push(None);
                }
                if let Some(ref t) = layer.mask_rgba {
                    let tex = self.renderer.upload_rgba(&t.rgba, t.width, t.height);
                    let uv = t.content_uv_scale();
                    self.we_mask_tex.push(Some(Texture {
                        tex,
                        w: t.width,
                        h: t.height,
                        uv_scale: uv,
                    }));
                } else {
                    self.we_mask_tex.push(None);
                }
                if let Some(ref t) = layer.phase_rgba {
                    let tex = self.renderer.upload_rgba(&t.rgba, t.width, t.height);
                    let uv = t.content_uv_scale();
                    self.we_phase_tex.push(Some(Texture {
                        tex,
                        w: t.width,
                        h: t.height,
                        uv_scale: uv,
                    }));
                } else {
                    self.we_phase_tex.push(None);
                }
            }
        }
        self.we_content = Some(content);
        self.we_dir = Some(dir.clone());
        self.we_started = Instant::now();
        // Restore per-wallpaper presentation (pause/rate/fit/zoom/flip…)
        // including any per-monitor visual overrides.
        let id = Self::workshop_id_for_dir(&dir);
        let stored = present::WePresent::load_file_for_id(&id);
        self.we_present = stored.base;
        self.we_present_mon = stored.monitors;
        self.sync_present_to_videos(
            !self.we_present.paused,
            self.we_present.rate.clamp(0.05, 4.0),
        );
        self.last_scene_tick = Instant::now();
        if for_editor {
            // Active we_* fields now hold the editor scene — park it in
            // editor_slot and restore the live desktop wallpaper.
            self.we_monitors.clear(); // never bind editor content to outputs
            if let Some(prev) = self.editor_slot.take() {
                self.free_bundle(prev);
            }
            self.editor_slot = self.take_active_bundle();
            if !desktop_stash.is_empty() {
                self.restore_we_bundles(desktop_stash);
            }
            return Ok(());
        }

        // Target monitors for this package (honour `we <monitor> <path>`).
        // Other monitors' packages are already sitting in `we_parked`.
        self.we_monitors = parse_monitor_list(monitor);
        log::info!(
            "WE package on monitors: {}",
            if self.we_monitors_is_all() {
                "*".into()
            } else {
                self.we_monitors.join(",")
            }
        );

        let ids: Vec<_> = self.outputs.keys().cloned().collect();
        for id in ids {
            let targeted = self.we_targets_output(&id);
            if targeted {
                if let Some(out) = self.outputs.get_mut(&id) {
                    out.current_path = Some(dir.clone());
                    out.pending_path = None;
                    if let Some(wipe) = out.wipe.take() {
                        self.renderer.delete_texture(wipe.old.tex);
                        self.renderer.delete_texture(wipe.new.tex);
                    }
                    // Keep `out.current` (boot still / classic) as a poster until
                    // WE has a real drawable frame — deleting it here caused a
                    // black flash while video/scene buffers the first frame.
                    if let Some(pos) = we_stills.iter().position(|(sid, _)| *sid == id) {
                        let (_, tex) = we_stills.swap_remove(pos);
                        let now = Instant::now();
                        out.wipe_we = Some(WipeWe {
                            tex,
                            start: now,
                            last_draw: now,
                            dur_ms: self.cfg.wipe_ms,
                            feather_px: self.cfg.wipe_feather_px,
                        });
                        out.wipe_pending_first = true;
                    }
                }
                self.draw_output(id);
            } else if prev_we_outs.contains(&id) {
                // Was showing WE on this output. Another slot may still own it
                // (different wallpaper on the other monitor) — only fall back to
                // classic config when nothing WE remains for this name.
                let still_we = self
                    .outputs
                    .get(&id)
                    .map(|o| self.we_owns_monitor(&o.info.name))
                    .unwrap_or(false);
                if still_we {
                    self.draw_output(id);
                } else {
                    self.apply_config_to(id, Some(Transition::Snap));
                }
            } else {
                // Untouched monitor — redraw whatever classic frame it already has.
                self.draw_output(id);
            }
        }
        // Persist so the next boot / walld restart restores this layout.
        if !for_editor {
            self.save_we_session();
            // Offline still seed (ffmpeg / preview) — no GPU readback.
            self.seed_boot_stills_from_we_dir(&dir);
            self.arm_boot_capture(); // currently a no-op for GPU capture
        }
        Ok(())
    }

    /// Write boot stills from the WE package on disk (ffmpeg first frame or preview).
    /// Avoids GL readback which was freezing presents on AMD/Mesa.
    fn seed_boot_stills_from_we_dir(&self, dir: &Path) {
        let mons: Vec<String> = self
            .outputs
            .values()
            .map(|o| o.info.name.clone())
            .filter(|n| !n.is_empty())
            .collect();
        if mons.is_empty() {
            return;
        }
        // Prefer a video file in the package, else preview.gif / preview.jpg.
        let mut src: Option<PathBuf> = None;
        if let Ok(rd) = std::fs::read_dir(dir) {
            for ent in rd.flatten() {
                let p = ent.path();
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_ascii_lowercase();
                if matches!(ext.as_str(), "mp4" | "webm" | "mkv") {
                    src = Some(p);
                    break;
                }
            }
        }
        if src.is_none() {
            for name in ["preview.gif", "preview.jpg", "preview.jpeg", "preview.png"] {
                let p = dir.join(name);
                if p.is_file() {
                    src = Some(p);
                    break;
                }
            }
        }
        let Some(src) = src else {
            log::debug!("boot-still: no preview/video in {}", dir.display());
            return;
        };
        // Decode via the image crate for stills; for video shell out to ffmpeg once.
        let ext = src
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(ext.as_str(), "mp4" | "webm" | "mkv") {
            let tmp = boot_still::cache_dir().join(".seed-frame.png");
            let _ = std::fs::create_dir_all(boot_still::cache_dir());
            let ok = std::process::Command::new("ffmpeg")
                .args([
                    "-y",
                    "-loglevel",
                    "error",
                    "-ss",
                    "1",
                    "-i",
                    &src.to_string_lossy(),
                    "-frames:v",
                    "1",
                    "-vf",
                    "scale=1920:-1",
                    &tmp.to_string_lossy(),
                ])
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok || !tmp.is_file() {
                log::debug!("boot-still: ffmpeg seed failed for {}", src.display());
                return;
            }
            for m in &mons {
                if let Err(e) = boot_still::save_from_path(m, &tmp) {
                    log::warn!("boot-still: seed {m}: {e}");
                }
            }
            let _ = std::fs::remove_file(&tmp);
        } else {
            for m in &mons {
                if let Err(e) = boot_still::save_from_path(m, &src) {
                    log::warn!("boot-still: seed {m}: {e}");
                }
            }
        }
    }

    fn sync_present_to_videos(&self, playing: bool, rate: f32) {
        for d in self.we_layer_video.iter().flatten() {
            d.set_user_playing(playing);
            d.set_rate(rate);
        }
        if let Some(we_runtime::WeContent::Video { decoder, .. }) = &self.we_content {
            decoder.set_user_playing(playing);
            decoder.set_rate(rate);
        }
    }

    fn handle_we_present(&mut self, args: &[String]) -> String {
        if args.is_empty() {
            return self.we_present.status_line();
        }
        // Optional first arg: monitor name scopes visual present to that display
        // even when one package is on all monitors (`we_present DP-2 flip_h 1`).
        let mut args = args.to_vec();
        let mon_scope = if !args.is_empty() && self.looks_like_monitor(&args[0]) {
            Some(args.remove(0))
        } else {
            None
        };
        // `*` means "all displays / clear overrides" — not a single output.
        let mon_scope = mon_scope.filter(|m| m != "*");

        if args.is_empty() {
            return if let Some(ref m) = mon_scope {
                if self.ensure_we_active_for_monitor(m) {
                    self.effective_present_for(m).status_line()
                } else {
                    format!("err no WE on {m}")
                }
            } else {
                self.we_present.status_line()
            };
        }
        if let Some(ref m) = mon_scope {
            if !self.ensure_we_active_for_monitor(m) {
                return format!("err no WE on {m}");
            }
        }
        let key = args[0].as_str();
        if key == "reset" {
            if let Some(ref m) = mon_scope {
                // Reset only this monitor's visual override (back to base).
                self.we_present_mon.remove(m);
            } else {
                self.we_present = present::WePresent::default();
                self.we_present_mon.clear();
            }
        } else {
            let value = if args.len() > 1 {
                args[1..].join(" ")
            } else {
                return format!("err we_present {key} needs a value");
            };
            if let Some(ref m) = mon_scope {
                if present::is_playback_key(key) {
                    // One decoder — pause/rate/mute still shared.
                    if let Err(e) = self.we_present.apply_kv(key, &value) {
                        return format!("err {e}");
                    }
                } else if present::is_visual_key(key) {
                    // Per-display visual: start from effective, apply, store override.
                    let mut p = self.effective_present_for(m);
                    if let Err(e) = p.apply_kv(key, &value) {
                        return format!("err {e}");
                    }
                    p.clamp();
                    // Keep only visual fields in the override map (playback stays on base).
                    let mut vis = present::WePresent::default();
                    vis.copy_visual_from(&p);
                    // Preserve playback from base in the stored copy for status merge.
                    vis.paused = self.we_present.paused;
                    vis.rate = self.we_present.rate;
                    vis.mute = self.we_present.mute;
                    self.we_present_mon.insert(m.clone(), vis);
                } else if let Err(e) = self.we_present.apply_kv(key, &value) {
                    return format!("err {e}");
                }
            } else {
                // No monitor scope: update shared base. Visual keys also clear
                // per-monitor overrides so "all displays" stays in sync.
                if let Err(e) = self.we_present.apply_kv(key, &value) {
                    return format!("err {e}");
                }
                if present::is_visual_key(key) {
                    self.we_present_mon.clear();
                }
            }
        }
        self.we_present.clamp();
        // Avoid a huge dt on the first frame after unpause.
        self.last_scene_tick = Instant::now();
        self.sync_present_to_videos(
            !self.we_present.paused,
            self.we_present.rate.clamp(0.05, 4.0),
        );
        // Persist base + per-monitor visuals against the package id.
        if let Some(dir) = self.we_dir.clone() {
            let id = Self::workshop_id_for_id_or_dir(&dir);
            let _ = self
                .we_present
                .save_file_for_id(&id, &self.we_present_mon);
        }
        let reply = if let Some(ref m) = mon_scope {
            self.effective_present_for(m).status_line()
        } else {
            self.we_present.status_line()
        };
        // Redraw so fit/zoom/flip apply immediately (even while paused).
        let ids: Vec<_> = self.outputs.keys().cloned().collect();
        for id in ids {
            let _ = self.draw_output(id);
        }
        if let Some(ref m) = mon_scope {
            let _ = self.ensure_we_active_for_monitor(m);
        }
        reply
    }

    fn looks_like_monitor(&self, s: &str) -> bool {
        if s == "*" {
            return true;
        }
        // Known connected output, or a typical DRM/hypr name (DP-1, HDMI-A-1, eDP-1…).
        if self.outputs.values().any(|o| o.info.name.eq_ignore_ascii_case(s)) {
            return true;
        }
        let u = s.to_ascii_uppercase();
        u.starts_with("DP-")
            || u.starts_with("HDMI")
            || u.starts_with("EDP")
            || u.starts_with("DVI")
            || u.starts_with("VGA")
            || u.starts_with("WL-")
    }

    fn ensure_we_active_for_monitor(&mut self, name: &str) -> bool {
        if self.we_content.is_some() && bundle_targets_name(&self.we_monitors, name) {
            return true;
        }
        let Some(i) = self
            .we_parked
            .iter()
            .position(|b| bundle_targets_name(&b.monitors, name))
        else {
            return false;
        };
        if let Some(cur) = self.take_active_bundle() {
            self.we_parked.push(cur);
        }
        let b = self.we_parked.remove(i);
        self.install_bundle(b);
        true
    }

    fn workshop_id_for_id_or_dir(dir: &Path) -> String {
        Self::workshop_id_for_dir(dir)
    }

    fn we_needs_anim(&self) -> bool {
        self.any_we_needs_anim()
    }

    fn editor_frame_path() -> PathBuf {
        let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(run).join("walld-editor-preview.png")
    }

    fn editor_capture_size(&self) -> (u32, u32) {
        let max = self.editor_max_edge.max(160) as f32;
        // Prefer the editor slot's ortho when present (status IPC without swap).
        let (ow, oh) = if let Some(ed) = &self.editor_slot {
            match &ed.content {
                we_runtime::WeContent::Scene { runtime } => (
                    runtime.ortho_width.max(1.0),
                    runtime.ortho_height.max(1.0),
                ),
                _ => (16.0, 9.0),
            }
        } else {
            match &self.we_content {
                Some(we_runtime::WeContent::Scene { runtime }) => (
                    runtime.ortho_width.max(1.0),
                    runtime.ortho_height.max(1.0),
                ),
                _ => (16.0, 9.0),
            }
        };
        let scale = max / ow.max(oh);
        let w = (ow * scale).round().max(1.0) as u32;
        let h = (oh * scale).round().max(1.0) as u32;
        (w, h)
    }

    /// Paint one editor preview frame via the real WE GPU path (effects included).
    /// Caller must have the editor scene installed in the active `we_*` fields.
    fn capture_editor_frame(&mut self) -> bool {
        if self.we_content.is_none() {
            return false;
        }
        let Some(id) = self.outputs.keys().next().cloned() else {
            return false;
        };
        self.editor_capture_active = true;
        let ok = self.draw_we_on(id);
        self.editor_capture_active = false;
        ok
    }

    /// Swap editor_slot into active, capture one frame, swap desktop back.
    fn capture_editor_frame_standalone(&mut self) -> bool {
        let Some(editor) = self.editor_slot.take() else {
            return false;
        };
        let desktop = self.take_active_bundle();
        self.install_bundle(editor);
        let ok = self.capture_editor_frame();
        self.editor_slot = self.take_active_bundle();
        if let Some(d) = desktop {
            self.install_bundle(d);
        }
        ok
    }

    /// Still-capture the WE frame currently shown on `out_id` into an FBO texture
    /// (`we_still_ready`) so a wipe can blend old→new. Called by
    /// `load_we_package_ex` BEFORE the old bundle is drained. Uses the same
    /// offscreen-FBO route as editor capture — window readback (`read_rgba(None)`)
    /// is the AMD/Mesa WE-video freeze hazard.
    fn still_capture_we_for(
        &mut self,
        out_id: &wayland_client::backend::ObjectId,
    ) -> Option<glow::Texture> {
        let name = self
            .outputs
            .get(out_id)
            .map(|o| o.info.name.clone())
            .unwrap_or_default();
        if name.is_empty() {
            return None;
        }
        let owner_idx = self
            .we_parked
            .iter()
            .position(|b| bundle_targets_name(&b.monitors, &name));
        let active_owns = self.we_content.is_some()
            && bundle_targets_name(&self.we_monitors, &name);

        // If the owner is a parked slot, swap it into the active `we_*` fields
        // for the capture (mirrors the tick's bundle rotation); the active
        // bundle stashes aside for the duration.
        let stash = if let Some(i) = owner_idx {
            let active = self.take_active_bundle();
            let owner = self.we_parked.remove(i);
            self.install_bundle(owner);
            Some((i, active))
        } else if active_owns {
            None
        } else {
            return None; // nothing WE owns this output
        };

        self.we_still_ready = None;
        self.we_still_capture_pending = true;
        let _ = self.draw_we_on(out_id.clone());
        self.we_still_capture_pending = false;

        if let Some((i, active)) = stash {
            if let Some(owner) = self.take_active_bundle() {
                self.we_parked.insert(i.min(self.we_parked.len()), owner);
            }
            if let Some(b) = active {
                self.install_bundle(b);
            }
        }
        self.we_still_ready.take()
    }

    fn handle_we_editor(&mut self, args: &[String]) -> String {
        let Some(sub) = args.first().map(|s| s.as_str()) else {
            return "err usage: we_editor load|reload|stop|status …".into();
        };
        match sub {
            "status" => {
                let path = Self::editor_frame_path();
                let (w, h) = self.editor_capture_size();
                let dir = self
                    .editor_slot
                    .as_ref()
                    .map(|b| b.dir.display().to_string())
                    .unwrap_or_default();
                format!(
                    "ok gen={} w={} h={} path={} active={} dir={}",
                    self.editor_frame_gen,
                    w,
                    h,
                    path.display(),
                    self.editor_slot.is_some() as u8,
                    dir
                )
            }
            "stop" => {
                // Drop editor preview only — desktop WE keeps playing.
                self.clear_editor_slot();
                "ok editor stopped".into()
            }
            "reload" => {
                let Some(dir) = self.editor_slot.as_ref().map(|b| b.dir.clone()) else {
                    return "err no editor scene loaded".into();
                };
                match self.load_we_package_editor(&dir) {
                    Ok(()) => {
                        let _ = self.capture_editor_frame_standalone();
                        format!("ok reloaded gen={}", self.editor_frame_gen)
                    }
                    Err(e) => format!("err {e}"),
                }
            }
            "load" => {
                let Some(path) = args.get(1) else {
                    return "err usage: we_editor load <path> [max_edge]".into();
                };
                if let Some(edge) = args.get(2).and_then(|s| s.parse::<u32>().ok()) {
                    self.editor_max_edge = edge.clamp(160, 1920);
                }
                let path = Self::expand_user_path(path);
                match self.load_we_package_editor(&path) {
                    Ok(()) => {
                        let _ = self.capture_editor_frame_standalone();
                        format!(
                            "ok loaded gen={} path={}",
                            self.editor_frame_gen,
                            Self::editor_frame_path().display()
                        )
                    }
                    Err(e) => format!("err {e}"),
                }
            }
            other => format!("err unknown we_editor subcommand '{other}'"),
        }
    }

    /// Load a WE scene into the editor preview slot. Desktop wallpaper is
    /// left running on its monitors.
    fn load_we_package_editor(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.load_we_package_ex(path, true, "*")
    }

    fn expand_user_path(path: &str) -> PathBuf {
        let p = path.trim();
        if let Some(rest) = p.strip_prefix("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            return PathBuf::from(home).join(rest);
        }
        PathBuf::from(p)
    }

    /// Present rate: at least scene_fps, raised to video source fps when present.
    fn we_present_fps(&self) -> f32 {
        let mut fps = self.cfg.scene_fps.max(5) as f32;
        for d in self.we_layer_video.iter().flatten() {
            fps = fps.max(d.source_fps());
        }
        if let Some(we_runtime::WeContent::Video { decoder, .. }) = &self.we_content {
            fps = fps.max(decoder.source_fps());
        }
        fps.clamp(5.0, 120.0)
    }

    /// Pull latest frames from embedded scene video textures.
    /// Mpv: render into FBO (LWE). Ffmpeg: texSubImage2D from CPU RGBA.
    fn pump_we_layer_videos(&mut self) {
        // mpv/ffmpeg GL uploads require a current EGL context (not just a live
        // glow handle). Without this, empty black FBOs replace good posters.
        if self.renderer.make_current().is_err() {
            return;
        }
        let n = self.we_layer_video.len();
        for i in 0..n {
            let is_mpv = self.we_layer_video[i]
                .as_ref()
                .map(|d| d.is_mpv())
                .unwrap_or(false);
            if is_mpv {
                // Don't delete textures owned by the mpv decoder.
                // `render_mpv` returns None until a real frame is ready so we
                // keep the TEX poster instead of installing a blank FBO.
                let rendered = self.we_layer_video[i]
                    .as_mut()
                    .and_then(|d| d.render_mpv(&self.renderer.gl));
                if let Some((tex, w, h)) = rendered {
                    let slot = Texture {
                        tex,
                        w,
                        h,
                        uv_scale: (1.0, 1.0),
                    };
                    // Marker: we must not delete this tex on replace — mpv owns it.
                    // Store a copy of the handle for drawing only.
                    if i < self.we_layer_tex.len() {
                        // If previous was ffmpeg/poster-owned, free it.
                        if let Some(old) = self.we_layer_tex[i].take() {
                            // Only delete if it wasn't the same mpv tex.
                            if old.tex != tex {
                                self.renderer.delete_texture(old.tex);
                            }
                        }
                        self.we_layer_tex[i] = Some(slot);
                    } else {
                        self.we_layer_tex.push(Some(slot));
                    }
                }
                continue;
            }

            let Some(frame) = self.we_layer_video[i]
                .as_ref()
                .and_then(|d| d.try_frame())
            else {
                continue;
            };
            let reuse = self
                .we_layer_tex
                .get(i)
                .and_then(|t| t.as_ref())
                .filter(|t| t.w == frame.width && t.h == frame.height)
                .map(|t| t.tex);
            if let Some(tex) = reuse {
                self.renderer
                    .update_rgba(tex, &frame.rgba, frame.width, frame.height);
                continue;
            }
            if let Some(Some(old)) = self.we_layer_tex.get(i) {
                self.renderer.delete_texture(old.tex);
            }
            let tex = self
                .renderer
                .upload_rgba_video(&frame.rgba, frame.width, frame.height);
            let slot = Texture {
                tex,
                w: frame.width,
                h: frame.height,
                uv_scale: (1.0, 1.0),
            };
            if i < self.we_layer_tex.len() {
                self.we_layer_tex[i] = Some(slot);
            } else {
                self.we_layer_tex.push(Some(slot));
            }
        }
    }

    /// Top-level WE Video wallpaper frame pump (shared across monitors).
    fn pump_we_video_wallpaper(&mut self) {
        if self.renderer.make_current().is_err() {
            return;
        }
        let is_mpv = matches!(
            &self.we_content,
            Some(we_runtime::WeContent::Video { decoder, .. }) if decoder.is_mpv()
        );
        if is_mpv {
            let rendered = match self.we_content.as_mut() {
                Some(we_runtime::WeContent::Video { decoder, .. }) => {
                    decoder.render_mpv(&self.renderer.gl)
                }
                _ => None,
            };
            if let Some((tex, w, h)) = rendered {
                if let Some(Some(old)) = self.we_layer_tex.first() {
                    if old.tex != tex {
                        self.renderer.delete_texture(old.tex);
                    }
                }
                let slot = Texture {
                    tex,
                    w,
                    h,
                    uv_scale: (1.0, 1.0),
                };
                if self.we_layer_tex.is_empty() {
                    self.we_layer_tex.push(Some(slot));
                } else {
                    self.we_layer_tex[0] = Some(slot);
                }
            }
            return;
        }

        let Some(frame) = (match &self.we_content {
            Some(we_runtime::WeContent::Video { decoder, .. }) => decoder.try_frame(),
            _ => None,
        }) else {
            return;
        };
        let reuse = self
            .we_layer_tex
            .first()
            .and_then(|t| t.as_ref())
            .filter(|t| t.w == frame.width && t.h == frame.height)
            .map(|t| t.tex);
        if let Some(tex) = reuse {
            self.renderer
                .update_rgba(tex, &frame.rgba, frame.width, frame.height);
            return;
        }
        if let Some(Some(old)) = self.we_layer_tex.first() {
            self.renderer.delete_texture(old.tex);
        }
        let tex = self
            .renderer
            .upload_rgba_video(&frame.rgba, frame.width, frame.height);
        let slot = Texture {
            tex,
            w: frame.width,
            h: frame.height,
            uv_scale: (1.0, 1.0),
        };
        if self.we_layer_tex.is_empty() {
            self.we_layer_tex.push(Some(slot));
        } else {
            self.we_layer_tex[0] = Some(slot);
        }
    }

    fn draw_we_on(&mut self, out_id: wayland_client::backend::ObjectId) -> bool {
        let Some(out) = self.outputs.get(&out_id) else {
            log_we_draw_bail("output gone");
            return false;
        };
        let Some(s) = out.surface.as_ref() else {
            log_we_draw_bail("no layer surface yet");
            return false;
        };
        let Some(win) = s.egl_window.as_ref() else {
            log_we_draw_bail("no egl window yet (configure not received)");
            return false;
        };
        if s.width == 0 || s.height == 0 {
            log_we_draw_bail("surface has zero size");
            return false;
        }
        let surf_w = s.width as i32;
        let surf_h = s.height as i32;
        if self.renderer.attach_window(win).is_err() {
            log_we_draw_bail("eglMakeCurrent on the wallpaper surface failed");
            return false;
        }

        let scene_clear = match &self.we_content {
            Some(we_runtime::WeContent::Scene { runtime }) => {
                let c = runtime.clear_color;
                [c[0], c[1], c[2], 1.0]
            }
            _ => [0.0, 0.0, 0.0, 1.0],
        };

        // Editor GPU capture / wipe-still capture: same shaders/layers as the
        // wallpaper, offscreen FBO (never the window — readback freezes WE video).
        let (vw, vh) = if self.editor_capture_active {
            let (ew, eh) = self.editor_capture_size();
            let key = (ew, eh);
            let recreate = match &self.editor_fbo {
                Some((sz, _)) => *sz != key,
                None => true,
            };
            if recreate {
                if let Some((_, (f, t))) = self.editor_fbo.take() {
                    self.renderer.delete_target(f, t);
                }
                if let Some(rt) = self.renderer.create_target(key.0, key.1) {
                    self.editor_fbo = Some((key, rt));
                }
            }
            let Some((_, (fbo, _tex))) = self.editor_fbo else {
                return false;
            };
            self.renderer
                .bind_draw_target(Some((fbo, key.0, key.1)), surf_w, surf_h, true);
            self.renderer.clear_bound(scene_clear);
            (key.0 as i32, key.1 as i32)
        } else if self.we_still_capture_pending {
            // Full-surface still of the outgoing WE frame.
            let key = (surf_w.max(1) as u32, surf_h.max(1) as u32);
            let recreate = match &self.we_still_fbo {
                Some((sz, _)) => *sz != key,
                None => true,
            };
            if recreate {
                if let Some((_, (f, t))) = self.we_still_fbo.take() {
                    self.renderer.delete_target(f, t);
                }
                if let Some(rt) = self.renderer.create_target(key.0, key.1) {
                    self.we_still_fbo = Some((key, rt));
                }
            }
            let Some((_, (fbo, _tex))) = self.we_still_fbo else {
                return false;
            };
            self.renderer
                .bind_draw_target(Some((fbo, key.0, key.1)), surf_w, surf_h, true);
            self.renderer.clear_bound(scene_clear);
            (key.0 as i32, key.1 as i32)
        } else {
            self.renderer.begin_frame(surf_w, surf_h, scene_clear);
            (surf_w, surf_h)
        };

        // Video textures are pumped once in tick_animations (shared).

        // Per-output visual present (flip/zoom/…) so all-monitors play still
        // allows independent layout on DP-1 vs DP-2.
        let out_name = self
            .outputs
            .get(&out_id)
            .map(|o| o.info.name.clone())
            .unwrap_or_default();
        let present_here = self.effective_present_for(&out_name);

        if let Some(we_runtime::WeContent::Video { .. }) = &self.we_content {
            // Gate that keeps the video's very first (possibly black) frame
            // from showing during a WE wipe: the overlay block of that draw
            // paints the outgoing still over the whole surface instead, then
            // clears the flag once it has drawn atop real content.
            let pending_first = self
                .outputs
                .get(&out_id)
                .map(|o| o.wipe_pending_first)
                .unwrap_or(false)
                && self.we_layer_tex.first().map_or(true, |t| t.is_none());
            if let Some(Some(tex)) = self.we_layer_tex.first() {
                if pending_first {
                    log::debug!("WE video first frame held for wipe overlay");
                } else {
                    let (tt, tw, th) = (tex.tex, tex.w, tex.h);
                    let p = &present_here;
                    self.renderer.draw_blit_present(
                        vw,
                        vh,
                        tt,
                        tw,
                        th,
                        p.fit.to_fit_mode(),
                        p.zoom,
                        (p.offset_x, p.offset_y),
                        p.flip_h,
                        p.flip_v,
                    );
                }
            } else if pending_first {
                // No frame yet, but the wipe overlay paints the outgoing still
                // over this output — not a black hole.
            } else {
                // Cleared to black above and nothing drawn over it: the decoder
                // has not handed us a first frame. Steady-state here means a
                // black wallpaper, not a poster.
                log_we_draw_bail("video has no decoded frame yet (painting black)");
            }
        } else if matches!(&self.we_content, Some(we_runtime::WeContent::Scene { .. })) {
            // Snapshot draw data (interleaved image/particle order + puppet tris).
            let (ortho_w, ortho_h, time, draw_list, puppet_batches, debug_marks) = {
                let runtime = match &self.we_content {
                    Some(we_runtime::WeContent::Scene { runtime }) => runtime,
                    _ => unreachable!(),
                };
                let time = runtime.time();
                let draw_list = runtime.scene_draw_list();
                let mut puppet_batches: Vec<(usize, Vec<[f32; 4]>)> = Vec::new();
                for (i, layer) in runtime.images.iter().enumerate() {
                    if let Some(ref mesh) = layer.puppet {
                        let tris = mesh.to_camera_tris_crop(
                            [layer.origin[0], layer.origin[1]],
                            [layer.scale[0], layer.scale[1]],
                            layer.angles[2],
                            layer.crop_offset,
                            layer.size,
                        );
                        if !tris.is_empty() {
                            puppet_batches.push((i, tris));
                        }
                    }
                }
                let debug_marks: Vec<(f32, f32)> = runtime
                    .debug_clicks
                    .iter()
                    .map(|c| (c.cam_x, c.cam_y))
                    .collect();
                (
                    runtime.ortho_width,
                    runtime.ortho_height,
                    time,
                    draw_list,
                    puppet_batches,
                    debug_marks,
                )
            };

            // util/composelayer needs a readable colour buffer. Sampling the
            // Wayland EGL window FB yields black/garbage on Mesa, so when any
            // compose layer is present we render the whole scene into an
            // offscreen RT and blit it out at the end.
            let needs_scene_rt = draw_list.iter().any(|item| {
                matches!(
                    item,
                    wallengine_we::SceneDrawItem::Image(d) if d.composelayer
                )
            });
            let scene_rt = if needs_scene_rt {
                let key = (vw as u32, vh as u32);
                let recreate = match &self.we_scene_rt {
                    Some((sz, _)) => *sz != key,
                    None => true,
                };
                if recreate {
                    if let Some((_, (f, t))) = self.we_scene_rt.take() {
                        self.renderer.delete_target(f, t);
                    }
                    if let Some(rt) = self.renderer.create_target(key.0, key.1) {
                        self.we_scene_rt = Some((key, rt));
                    }
                }
                if let Some((_, (fbo, tex))) = self.we_scene_rt {
                    self.renderer
                        .bind_draw_target(Some((fbo, key.0, key.1)), vw, vh, true);
                    // Clear the scene RT (begin_frame already cleared the window).
                    self.renderer.clear_bound(scene_clear);
                    Some((fbo, tex, key.0, key.1))
                } else {
                    None
                }
            } else if self.editor_capture_active || self.we_still_capture_pending {
                // Editor / still FBO already bound as home above — stay there.
                None
            } else {
                self.renderer.bind_draw_target(None, vw, vh, true);
                None
            };
            for item in &draw_list {
                match item {
                    wallengine_we::SceneDrawItem::Image(d) => {
                        let mut d = d.clone();
                        // util/composelayer: sample the scene RT under this
                        // layer's screen rect, run pixelate/vhs/pulse, draw.
                        if d.composelayer {
                            if !wallengine_we::gl_blend_mode_supported(d.color_blend_mode) {
                                continue;
                            }
                            let Some((scene_fbo, _scene_tex, sw, sh)) = scene_rt else {
                                continue;
                            };
                            let (s, _, _) = wallengine_we::cover_fit(
                                ortho_w,
                                ortho_h,
                                vw as f32,
                                vh as f32,
                            );
                            // Layer pixel size tracks on-screen extent so the
                            // censor region matches the TV overlay drawn next.
                            let lw = (d.size[0].abs() * s).round().max(1.0) as u32;
                            let lh = (d.size[1].abs() * s).round().max(1.0) as u32;
                            // Snapshot scene RT into a free texture — sampling the
                            // live colour attachment is black on Mesa.
                            let Some((snap_fbo, snap_tex)) =
                                self.renderer.create_target(sw, sh)
                            else {
                                continue;
                            };
                            self.renderer.blit_fbo_to_fbo(scene_fbo, snap_fbo, sw, sh);
                            let Some((layer_fbo, layer_tex)) =
                                self.renderer.create_target(lw, lh)
                            else {
                                self.renderer.delete_target(snap_fbo, snap_tex);
                                continue;
                            };
                            self.renderer.sample_compose_layer(
                                layer_fbo,
                                lw,
                                lh,
                                snap_tex,
                                d.origin,
                                d.size,
                                d.angle_z,
                                ortho_w,
                                ortho_h,
                                vw,
                                vh,
                            );
                            self.renderer.delete_target(snap_fbo, snap_tex);
                            let mut albedo = Texture {
                                tex: layer_tex,
                                w: lw,
                                h: lh,
                                uv_scale: (1.0, 1.0),
                            };
                            let owned_input = albedo.tex;
                            if self.we_fx_progs.contains_key(&d.layer_index) {
                                if let Some(fx) =
                                    self.run_layer_effects(d.layer_index, albedo, time)
                                {
                                    d.rebase_texture_uv(albedo.uv_scale, fx.image.uv_scale);
                                    albedo = Texture { tex: fx.image.tex, w: fx.image.w, h: fx.image.h, uv_scale: fx.image.uv_scale };
                                    d.suppress_applied_effects(&fx.applied);
                                }
                            }
                            // Home FBO (scene RT) is restored by sample/effects.
                            self.renderer.restore_draw_home(vw, vh);
                            self.renderer.draw_ortho_image(
                                vw,
                                vh,
                                ortho_w,
                                ortho_h,
                                albedo.tex,
                                d.origin,
                                d.size,
                                d.angle_z,
                                d.alpha,
                                (1.0, 1.0),
                                (0.0, 0.0),
                                d.colorkey,
                                d.color_blend_mode,
                            );
                            self.renderer.delete_target(layer_fbo, owned_input);
                            continue;
                        }
                        let Some(Some(albedo)) = self.we_layer_tex.get(d.layer_index) else {
                            continue;
                        };
                        if !wallengine_we::gl_blend_mode_supported(d.color_blend_mode) {
                            continue;
                        }
                        // Run WE effect passes over the layer texture first
                        // (shake/waterwaves/ripple/… — this is what animates
                        // otherwise-still artwork).
                        let mut albedo = *albedo;
                        if self.we_fx_progs.contains_key(&d.layer_index) {
                            if let Some(fx) = self.run_layer_effects(d.layer_index, albedo, time) {
                                d.rebase_texture_uv(albedo.uv_scale, fx.image.uv_scale);
                                    albedo = Texture { tex: fx.image.tex, w: fx.image.w, h: fx.image.h, uv_scale: fx.image.uv_scale };
                                d.suppress_applied_effects(&fx.applied);
                            }
                        }
                        let albedo = &albedo;
                        // Puppet layers: draw mesh triangles instead of unit quad.
                        if d.has_puppet {
                            if let Some((_, tris)) =
                                puppet_batches.iter().find(|(i, _)| *i == d.layer_index)
                            {
                                self.renderer.draw_puppet_tris(
                                    vw,
                                    vh,
                                    ortho_w,
                                    ortho_h,
                                    albedo.tex,
                                    tris,
                                    (d.uv_scale[0], d.uv_scale[1]),
                                    (d.uv_offset[0], d.uv_offset[1]),
                                    d.alpha, d.colorkey, d.color_blend_mode,
                                );
                                continue;
                            }
                        }
                        let has_wf = d.has_waterflow
                            && self
                                .we_mask_tex
                                .get(d.layer_index)
                                .and_then(|t| t.as_ref())
                                .is_some()
                            && self
                                .we_phase_tex
                                .get(d.layer_index)
                                .and_then(|t| t.as_ref())
                                .is_some();
                        let has_op = d.has_opacity
                            && self
                                .we_mask_tex
                                .get(d.layer_index)
                                .and_then(|t| t.as_ref())
                                .is_some();

                        if has_wf {
                            let mask = self.we_mask_tex[d.layer_index].as_ref().unwrap();
                            let phase = self.we_phase_tex[d.layer_index].as_ref().unwrap();
                            let wf = d.waterflow.as_ref().unwrap();
                            let (mu, mv) = mask.uv_scale;
                            self.renderer.draw_waterflow(
                                vw,
                                vh,
                                ortho_w,
                                ortho_h,
                                albedo.tex,
                                mask.tex,
                                phase.tex,
                                mask.w,
                                mask.h,
                                (mask.w as f32 * mu) as u32,
                                (mask.h as f32 * mv) as u32,
                                d.origin,
                                d.size,
                                d.angle_z,
                                time,
                                wf.speed,
                                wf.strength,
                                wf.phasescale,
                                wf.feather,
                                (d.uv_scale[0], d.uv_scale[1]),
                                (d.uv_offset[0], d.uv_offset[1]),
                            );
                        } else if has_op {
                            let mask = self.we_mask_tex[d.layer_index].as_ref().unwrap();
                            let alpha =
                                d.opacity.as_ref().map(|o| o.strength).unwrap_or(1.0) * d.alpha;
                            self.renderer.draw_opacity_masked(
                                vw,
                                vh,
                                ortho_w,
                                ortho_h,
                                albedo.tex,
                                mask.tex,
                                d.origin,
                                d.size,
                                d.angle_z,
                                alpha,
                                (d.uv_scale[0], d.uv_scale[1]),
                                (d.uv_offset[0], d.uv_offset[1]),
                                mask.uv_scale,
                            );
                        } else {
                            self.renderer.draw_ortho_image(
                                vw,
                                vh,
                                ortho_w,
                                ortho_h,
                                albedo.tex,
                                d.origin,
                                d.size,
                                d.angle_z,
                                d.alpha,
                                (d.uv_scale[0], d.uv_scale[1]),
                                (d.uv_offset[0], d.uv_offset[1]),
                                d.colorkey,
                                d.color_blend_mode,
                            );
                        }
                    }
                    wallengine_we::SceneDrawItem::Text(td) => {
                        let runtime = match &self.we_content {
                            Some(we_runtime::WeContent::Scene { runtime }) => runtime,
                            _ => continue,
                        };
                        let Some(txt) = runtime.texts.get(td.text_index) else {
                            continue;
                        };
                        let Some(ref bitmap) = txt.rgba else { continue };
                        let stale = !matches!(
                            self.we_text_tex.get(td.text_index),
                            Some(Some((_, gen))) if *gen == td.generation
                        );
                        if stale {
                            let tex = self.renderer.upload_rgba(
                                &bitmap.rgba,
                                bitmap.width,
                                bitmap.height,
                            );
                            if let Some(slot) = self.we_text_tex.get_mut(td.text_index) {
                                if let Some((old, _)) = slot.take() {
                                    self.renderer.delete_texture(old.tex);
                                }
                                *slot = Some((
                                    Texture {
                                        tex,
                                        w: bitmap.width,
                                        h: bitmap.height,
                                        uv_scale: (1.0, 1.0),
                                    },
                                    td.generation,
                                ));
                            }
                        }
                        let Some(Some((tex, _))) = self.we_text_tex.get(td.text_index) else {
                            continue;
                        };
                        self.renderer.draw_ortho_image(
                            vw,
                            vh,
                            ortho_w,
                            ortho_h,
                            tex.tex,
                            td.origin,
                            td.size,
                            0.0,
                            td.alpha,
                            (1.0, 1.0),
                            (0.0, 0.0),
                            None,
                            0,
                        );
                    }
                    wallengine_we::SceneDrawItem::Particle(pd) => {
                        let runtime = match &self.we_content {
                            Some(we_runtime::WeContent::Scene { runtime }) => runtime,
                            _ => continue,
                        };
                        let Some(sys) = runtime.particles.get(pd.system_index) else {
                            continue;
                        };
                        // Textured sprites when the material supplies a texture.
                        if let Some(Some(ptex)) = self.we_particle_tex.get(pd.system_index) {
                            let verts = sys.sprite_vertices(
                                ortho_w,
                                ortho_h,
                                vw as f32,
                                vh as f32,
                            );
                            if !verts.is_empty() {
                                self.renderer.draw_particle_sprites(
                                    vw,
                                    vh,
                                    ptex.tex,
                                    &verts,
                                    sys.is_additive(),
                                    sys.overbright.max(0.0),
                                );
                            }
                            continue;
                        }
                        let mut data = Vec::new();
                        let (fit_s, _, _) = wallengine_we::cover_fit(
                            ortho_w,
                            ortho_h,
                            vw as f32,
                            vh as f32,
                        );
                        // Use max XY scale for diameter so non-uniform snow
                        // (tiny X, large Y) still reads as large flakes.
                        // Soft point fallback: LWE size is full sprite width in local
                        // units (after sizerandom /2). Soft points need a small boost.
                        let sc = sys.scale[0]
                            .abs()
                            .max(sys.scale[1].abs())
                            .max(0.01);
                        // Soft point fallback has no overbright uniform — bake once.
                        let ob = sys.overbright.max(0.0);
                        let additive = sys.is_additive();
                        for p in sys.alive() {
                            let cam = sys.local_to_camera(p.pos);
                            let uv = wallengine_we::camera_to_viewport_uv(
                                cam[0],
                                cam[1],
                                ortho_w,
                                ortho_h,
                                vw as f32,
                                vh as f32,
                            );
                            let size_px = (p.size * sc * fit_s * 1.8).max(1.5);
                            let size_n = size_px / (vw as f32).min(vh as f32).max(1.0);
                            data.push(uv[0]);
                            data.push(uv[1]);
                            data.push(size_n.max(0.0015));
                            data.push(p.alpha);
                            data.push((p.color[0] * ob).min(4.0));
                            data.push((p.color[1] * ob).min(4.0));
                            data.push((p.color[2] * ob).min(4.0));
                        }
                        if !data.is_empty() {
                            self.renderer.draw_points_ex(vw, vh, &data, additive);
                        }
                    }
                }
            }

            // Debug click markers (yellow points)
            if !debug_marks.is_empty() {
                let mut data = Vec::new();
                for (cx, cy) in &debug_marks {
                    let uv = wallengine_we::camera_to_viewport_uv(
                        *cx, *cy, ortho_w, ortho_h, vw as f32, vh as f32,
                    );
                    data.push(uv[0]);
                    data.push(uv[1]);
                    data.push(0.02);
                    data.push(1.0);
                    data.push(1.0);
                    data.push(1.0);
                    data.push(0.2);
                }
                self.renderer.draw_points(vw, vh, &data);
            }

            // Present scene RT to the window (1:1, same GL orientation).
            if let Some((fbo, _, w, h)) = scene_rt {
                if self.editor_capture_active {
                    if let Some((_, (efbo, _))) = self.editor_fbo {
                        self.renderer.blit_fbo_to_fbo(fbo, efbo, w, h);
                    }
                } else if self.we_still_capture_pending {
                    if let Some((_, (sfbo, _))) = self.we_still_fbo {
                        self.renderer.blit_fbo_to_fbo(fbo, sfbo, w, h);
                    }
                } else {
                    self.renderer.blit_fbo_to_default(fbo, w, h);
                }
            }
        }

        // WE wipe overlay: paint the outgoing scene's last still over the live
        // incoming scene (already rendered above) behind the retreating diagonal
        // mask. Editor / still captures render to their own FBOs and must stay clean.
        if !self.editor_capture_active && !self.we_still_capture_pending {
            if let Some(out) = self.outputs.get_mut(&out_id) {
                let pending_first = out.wipe_pending_first;
                let done = if let Some(wipe) = out.wipe_we.as_mut() {
                    // Video incoming with no decoded frame yet: paint the old
                    // still as a full-screen hold frame (progress -1 → alpha 1
                    // everywhere) and pause the clock, so the wipe starts from
                    // t=0 the moment real frames arrive — never over black.
                    let holding = matches!(
                        self.we_content,
                        Some(we_runtime::WeContent::Video { .. })
                    ) && !pending_first
                        && self.we_layer_tex.first().map_or(true, |t| t.is_none());
                    let now = Instant::now();
                    let dt = now.saturating_duration_since(wipe.last_draw);
                    if holding {
                        wipe.start += dt;
                    }
                    wipe.last_draw = now;
                    let t = (wipe.start.elapsed().as_secs_f32() * 1000.0
                        / wipe.dur_ms.max(1) as f32)
                        .clamp(0.0, 1.0);
                    let progress = if holding {
                        -1.0
                    } else {
                        wayland::ease_out_cubic(t)
                    };
                    let (tex, feather_px) = (wipe.tex, wipe.feather_px);
                    self.renderer
                        .draw_we_overlay(vw, vh, tex, progress, feather_px);
                    t >= 1.0
                } else {
                    false
                };
                if done {
                    if let Some(w) = out.wipe_we.take() {
                        self.renderer.delete_texture(w.tex);
                    }
                }
                if pending_first {
                    out.wipe_pending_first = false;
                }
            }
        }

        if self.editor_capture_active {
            if let Some(((w, h), (fbo, _))) = self.editor_fbo {
                let rgba = self.renderer.read_rgba(fbo, w, h);
                let path = Self::editor_frame_path();
                match ::image::RgbaImage::from_raw(w, h, rgba) {
                    Some(img) => {
                        // Atomic write so the wallstudio poller never reads a
                        // half-written PNG (which freezes the live preview).
                        // Keep a real `.png` extension — the `image` crate keys
                        // the encoder off the path suffix.
                        let tmp = path.with_file_name("walld-editor-preview.tmp.png");
                        let saved = img
                            .save(&tmp)
                            .map_err(|e| e.to_string())
                            .and_then(|_| {
                                std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
                            });
                        match saved {
                            Ok(()) => {
                                self.editor_frame_gen = self.editor_frame_gen.wrapping_add(1);
                            }
                            Err(e) => {
                                let _ = std::fs::remove_file(&tmp);
                                log::warn!("editor preview save: {e}");
                            }
                        }
                    }
                    None => log::warn!("editor preview: bad rgba buffer size"),
                }
            }
            // Restore default FB binding; do not swap/commit the wallpaper surface.
            self.renderer.bind_draw_target(None, surf_w, surf_h, true);
            return true;
        }

        // Wipe-still capture: detach the just-rendered texture from its FBO and
        // hand it to the caller (Mesa returns black for textures still attached
        // to a live FBO). Restore the window binding and skip the swap — the
        // visible surface keeps its previously committed frame while in flight.
        if self.we_still_capture_pending {
            self.we_still_capture_pending = false;
            if let Some((_, (fbo, tex))) = self.we_still_fbo.take() {
                self.renderer.release_target(fbo, tex);
                self.we_still_ready = Some(tex);
            }
            self.renderer.bind_draw_target(None, surf_w, surf_h, true);
            return true;
        }

        self.renderer.swap();
        if let Some(out) = self.outputs.get_mut(&out_id) {
            if let Some(s) = out.surface.as_mut() {
                s.wl_surface.commit();
                s.committed = true;
            }
        }
        self.we_needs_anim()
    }
}

/// Parse `*` / empty / `DP-1` / `DP-1,DP-2` into a target list.
fn parse_monitor_list(s: &str) -> Vec<String> {
    let t = s.trim();
    if t.is_empty() || t == "*" {
        return vec!["*".into()];
    }
    t.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

// ── Wayland Dispatch impls ─────────────────────────────────────────────────

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Daemon {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global { name, interface, version } => {
                state.bind_global(registry, name, &interface, version);
            }
            wl_registry::Event::GlobalRemove { name } => {
                // An output may have been unplugged: drop it + its wallpaper state.
                let gone: Vec<_> = state
                    .outputs
                    .iter()
                    .filter(|(_, o)| o.global_name == name)
                    .map(|(id, _)| id.clone())
                    .collect();
                for id in gone {
                    state.destroy_surface(id.clone());
                    state.outputs.remove(&id);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for Daemon {
    fn event(
        _: &mut Self,
        _: &wl_compositor::WlCompositor,
        _: wl_compositor::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zwlr_layer_shell_v1::ZwlrLayerShellV1, ()> for Daemon {
    fn event(
        _: &mut Self,
        _: &zwlr_layer_shell_v1::ZwlrLayerShellV1,
        _: zwlr_layer_shell_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_surface::WlSurface, ()> for Daemon {
    fn event(
        _state: &mut Self,
        _surface: &wl_surface::WlSurface,
        _event: wl_surface::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        // wl_surface has no events we need (release arrives only at v6+).
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Daemon {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        event: wl_output::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let id = output.id();
        let Some(out) = state.outputs.get_mut(&id) else { return };
        match event {
            wl_output::Event::Name { name } => {
                out.info.name = name;
            }
            wl_output::Event::Mode { flags, width, height, .. } => {
                if let WEnum::Value(f) = flags {
                    if f.contains(wl_output::Mode::Current) {
                        out.info.width = width;
                        out.info.height = height;
                    }
                }
            }
            wl_output::Event::Scale { factor } => out.info.scale = factor as f64,
            wl_output::Event::Done => {
                log::debug!(
                    "output {}: {}x{} scale {}",
                    out.info.name,
                    out.info.width,
                    out.info.height,
                    out.info.scale
                );
            }
            _ => {}
        }
        state.ensure_surface(id);
    }
}

impl Dispatch<zwlr_layer_surface_v1::ZwlrLayerSurfaceV1, ()> for Daemon {
    fn event(
        state: &mut Self,
        layer: &zwlr_layer_surface_v1::ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        _: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        let Some(out_id) = state.surface_map.get(&layer.id()).cloned() else { return };
        match event {
            zwlr_layer_surface_v1::Event::Configure { serial, width, height } => {
                layer.ack_configure(serial);
                if width == 0 || height == 0 {
                    return;
                }
                let Some(out) = state.outputs.get_mut(&out_id) else { return };
                let Some(s) = out.surface.as_mut() else { return };
                s.width = width;
                s.height = height;
                match s.egl_window.as_ref() {
                    None => match wayland_egl::WlEglSurface::new(s.wl_surface.id(), width as i32, height as i32) {
                        Ok(win) => s.egl_window = Some(win),
                        Err(e) => log::error!("wl_egl_surface create failed: {e:?}"),
                    },
                    Some(win) => win.resize(width as i32, height as i32, 0, 0),
                }
                state.draw_output(out_id);
            }
            zwlr_layer_surface_v1::Event::Closed => {
                let Some(out) = state.outputs.get_mut(&out_id) else { return };
                if let Some(s) = out.surface.take() {
                    drop(s.egl_window);
                    s.layer.destroy();
                    s.wl_surface.destroy();
                }
                log::debug!("layer surface closed");
            }
            _ => {}
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // CLI: `walld ctl <command...>` forwards one line to the daemon.
    if args.len() >= 2 && args[1] == "ctl" {
        let line = args[2..].join(" ");
        if line.trim().is_empty() {
            eprintln!("usage: walld ctl <ping|status|reload|cfg_reload|set|snap|wipe|scene|we|we_stop|we_present|boot_capture|preload|stop|start|ready|quit>");
            std::process::exit(2);
        }
        std::process::exit(ipc::client_call(&line));
    }
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("walld — lightweight Hyprland wallpaper daemon\n\nusage: walld [-v]\n       walld ctl <command> ...");
        return;
    }

    let mut builder = env_logger::Builder::from_default_env();
    builder.format_timestamp_millis();
    builder.filter_level(if verbose { log::LevelFilter::Debug } else { log::LevelFilter::Info });
    builder.init();

    let cfg = config::load_global();
    log::info!(
        "walld starting (transition={:?}, wipe_ms={}, conf={})",
        cfg.transition,
        cfg.wipe_ms,
        cfg.hyprpaper_conf.display()
    );

    // Decode worker thread: keeps image decode off the render/event path.
    let (decode_tx, decode_rx) = std::sync::mpsc::channel::<PathBuf>();
    let (resp_tx, resp_ch): (channel::Sender<DecodeResp>, Channel<DecodeResp>) = channel::channel();
    std::thread::Builder::new()
        .name("walld-decode".into())
        .spawn(move || {
            for path in decode_rx {
                let res = image::decode_file(&path);
                if resp_tx.send(DecodeResp { path, res }).is_err() {
                    break; // daemon gone
                }
            }
        })
        .expect("spawn decode thread");

    // Wayland connection + event queue with globals collected.
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("walld: cannot connect to Wayland ({e}) — run this inside a Hyprland session");
            std::process::exit(1);
        }
    };
    let (globals, mut queue) = registry_queue_init::<Daemon>(&conn).expect("collect globals");

    // Event loop plumbing.
    let mut event_loop: EventLoop<Daemon> = EventLoop::try_new().expect("calloop event loop");
    let loop_signal = event_loop.get_signal();
    let handle = event_loop.handle();

    let mut daemon = Daemon::new(cfg, conn.clone(), queue.handle(), decode_tx).expect("init EGL/renderer");
    daemon.loop_signal = Some(loop_signal);

    // Bind globals that already exist (registry_queue_init's roundtrip filled the list).
    {
        let registry = globals.registry().clone();
        let list = globals.contents().clone_list();
        for g in list {
            daemon.bind_global(&registry, g.name, &g.interface, g.version);
        }
    }
    // Flush + roundtrip so wl_output name/mode/done arrive before we load walls.
    // Without this, outputs sit nameless and config never applies.
    if let Err(e) = queue.roundtrip(&mut daemon) {
        log::error!("initial wayland roundtrip failed: {e}");
        std::process::exit(1);
    }

    // Animation timer (deadline-driven timerfd; armed whenever a wipe runs).
    use nix::sys::timerfd::{ClockId, Expiration, TimerFd, TimerFlags, TimerSetTimeFlags};
    use std::rc::Rc;
    // Non-blocking: Level-triggered timerfd must not block the event loop.
    let timerfd = Rc::new(
        TimerFd::new(
            ClockId::CLOCK_MONOTONIC,
            TimerFlags::TFD_NONBLOCK | TimerFlags::TFD_CLOEXEC,
        )
        .expect("timerfd_create"),
    );
    let timer_raw_fd = timerfd.as_fd().as_raw_fd();
    // SAFETY: the timerfd outlives the event loop (Rc held by the callbacks).
    let timer_source = Generic::new(unsafe { FdWrapper::new(timer_raw_fd) }, Interest::READ, Mode::Level);
    handle
        .insert_source(timer_source, {
            let timerfd = timerfd.clone();
            move |_readiness, io_obj, state| {
                // Clear the expiration (may be multi-fire; drain fully).
                let mut buf = [0u8; 8];
                loop {
                    match nix::unistd::read(io_obj.as_raw_fd(), &mut buf) {
                        Ok(0) | Err(nix::errno::Errno::EAGAIN) => break,
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
                match state.tick_animations() {
                    Some(next) => {
                        let dur = next.saturating_duration_since(Instant::now()).max(Duration::from_millis(1));
                        let _ = timerfd.set(Expiration::OneShot(dur.into()), TimerSetTimeFlags::empty());
                    }
                    None => {
                        let _ = timerfd.unset();
                    }
                }
                Ok(PostAction::Continue)
            }
        })
        .expect("insert timer source");

    /// Arm the animation timer ~now if any wipe is in flight.
    fn arm_timer(daemon: &Daemon, timerfd: &TimerFd) {
        let need = daemon.scene_needs_anim()
            || daemon.we_needs_anim()
            || daemon.outputs.values().any(|o| o.wipe.is_some());
        if need {
            let _ = timerfd.set(
                Expiration::OneShot(Duration::from_millis(1).into()),
                TimerSetTimeFlags::empty(),
            );
        }
    }

    // Wayland socket: use Smithay's adapter. The previous hand-rolled Level +
    // prepare_read loop busy-spun under the libwayland (client_system) backend
    // that wayland-egl requires — it never drained correctly, so IPC hung and
    // outputs never received name/mode events.
    WaylandSource::new(conn, queue)
        .insert(handle.clone())
        .expect("insert wayland source");

    // Decode results.
    handle
        .insert_source(resp_ch, {
            let timerfd = timerfd.clone();
            move |event, (), state| {
                if let channel::Event::Msg(resp) = event {
                    state.handle_decode_result(resp);
                    arm_timer(state, &timerfd);
                }
            }
        })
        .expect("insert decode channel");

    // IPC socket.
    match ipc::IpcServer::start(&handle, {
        let timerfd = timerfd.clone();
        move |state: &mut Daemon, (stream, cmd)| {
            state.handle_ipc(cmd, stream);
            arm_timer(state, &timerfd);
        }
    }) {
        Ok(_) => log::info!("ipc: {}", ipc::sock_path().display()),
        Err(e) => {
            eprintln!("walld: {e}");
            std::process::exit(1);
        }
    }

    // Signals: SIGHUP reloads config; SIGTERM/SIGINT quit.
    handle
        .insert_source(
            Signals::new(&[Signal::SIGHUP, Signal::SIGTERM, Signal::SIGINT]).expect("signal source"),
            {
                let timerfd = timerfd.clone();
                move |ev: SignalEvent, (), state| {
                    match ev.signal() {
                        Signal::SIGHUP => {
                            log::info!("SIGHUP: reloading config");
                            state.reload(None);
                            arm_timer(state, &timerfd);
                        }
                        Signal::SIGTERM | Signal::SIGINT => {
                            log::info!("terminating");
                            state.running = false;
                            if let Some(s) = &state.loop_signal {
                                s.stop();
                            }
                        }
                        _ => {}
                    }
                }
            },
        )
        .expect("insert signal source");

    // Initial wallpaper load (uses configured default transition).
    daemon.reload(None);
    // Instant last-session stills *before* WE (covers Hyprland default bg while
    // workshop content loads). Overwrites hyprpaper conf paint if stills exist.
    daemon.apply_boot_stills();
    // Restore last WE layout (workshop videos/scenes). Static hyprpaper paths
    // alone leave a blank/static desktop after reboot if the user was on WE.
    daemon.restore_we_session();
    // NOTE: GPU boot-capture is suppressed (see arm_boot_capture). WE stills
    // should be seeded offline (ffmpeg/preview) when a pack is applied.
    arm_timer(&daemon, &timerfd);

    if let Err(e) = event_loop.run(None, &mut daemon, |_| {}) {
        log::error!("event loop: {e}");
    }

    // Cleanup: drop socket so a fresh start binds cleanly.
    let _ = std::fs::remove_file(ipc::sock_path());
    log::info!("walld exited");
}
