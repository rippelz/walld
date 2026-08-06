//! walld — lightweight Hyprland wallpaper daemon.
//!
//! One layer-shell surface per output, EGL/GLES drawing, IPC on
//! $XDG_RUNTIME_DIR/walld.sock, SIGHUP = reload config with the configured
//! transition. Static walls are GL textures; switches can snap or do a
//! GPU diagonal wipe (old→new half-plane blend).

mod config;
mod image;
mod ipc;
mod render;
mod wayland;

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
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
use wayland::{Output, OutputInfo, PendingWall, SurfaceState, Texture, WipeState, NAMESPACE};

/// Decoded image result coming back from the worker thread.
struct DecodeResp {
    path: PathBuf,
    res: Result<image::Image, String>,
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
        })
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
    }

    /// Queue the configured wallpaper for one output (skipped when unchanged).
    fn apply_config_to(&mut self, out_id: wayland_client::backend::ObjectId, transition_override: Option<Transition>) {
        let Some(name) = self.outputs.get(&out_id).map(|o| o.info.name.clone()) else {
            return;
        };
        if name.is_empty() {
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
        let new = Texture { tex, w: img.width, h: img.height };
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
        let ids: Vec<_> = self
            .outputs
            .iter()
            .filter(|(_, o)| o.wipe.is_some())
            .map(|(id, _)| id.clone())
            .collect();
        let mut next: Option<Instant> = None;
        for id in ids {
            if self.draw_output(id) {
                next = Some(Instant::now() + Duration::from_millis(8));
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
                let mut outs: Vec<_> = self.outputs.values().collect();
                outs.sort_by(|a, b| a.info.name.cmp(&b.info.name));
                for o in outs {
                    let state = if o.surface.is_none() {
                        "stopped"
                    } else if o.wipe.is_some() {
                        "wiping"
                    } else if o.pending_path.is_some() {
                        "loading"
                    } else if o.current.is_some() {
                        "shown"
                    } else {
                        "empty"
                    };
                    let path = o.current_path.as_deref().map(|p| p.display().to_string()).unwrap_or_default();
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
                "ok".to_string()
            }
            IpcCmd::Snap { monitor, path } => {
                self.set_wallpaper(&monitor, &path, Some(Transition::Snap));
                "ok".to_string()
            }
            IpcCmd::Wipe { monitor, path } => {
                self.set_wallpaper(&monitor, &path, Some(Transition::Wipe));
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
                let waiting = self
                    .outputs
                    .values()
                    .filter(|o| {
                        !o.surface.as_ref().is_some_and(|s| s.committed) || (o.current.is_none() && o.wipe.is_none())
                    })
                    .count();
                if waiting == 0 {
                    "ok ready".to_string()
                } else {
                    format!("err waiting:{waiting}")
                }
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
        if let Some(cur) = out.current.take() {
            self.renderer.delete_texture(cur.tex);
        }
        out.current_path = None;
        out.pending_path = None;
    }
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
            eprintln!("usage: walld ctl <ping|status|reload|set|snap|wipe|preload|stop|start|ready|quit>");
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
        if daemon.outputs.values().any(|o| o.wipe.is_some()) {
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
    arm_timer(&daemon, &timerfd);

    if let Err(e) = event_loop.run(None, &mut daemon, |_| {}) {
        log::error!("event loop: {e}");
    }

    // Cleanup: drop socket so a fresh start binds cleanly.
    let _ = std::fs::remove_file(ipc::sock_path());
    log::info!("walld exited");
}
