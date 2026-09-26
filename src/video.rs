//! In-engine video wallpaper decoder.
//!
//! **Primary (LWE-style):** `libmpv` + OpenGL render API (`video_mpv`) — hwdec,
//! timed present, mid-stream seek. No full-frame CPU RGBA churn.
//!
//! **Fallback:** system `ffmpeg` pipe (VAAPI or software), wall-clock paced from
//! the probed source frame rate.
//!
//! Force ffmpeg with `WALLD_VIDEO=ffmpeg`.

use std::io::{BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use base64::Engine as _;

use crate::video_mpv::{self, MpvGlDecoder};

pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoBackend {
    /// libmpv OpenGL render (LWE path).
    Mpv,
    Vaapi,
    Software,
    /// Chromium headless snapshot renderer for Wallpaper Engine web projects.
    Web,
}

impl VideoBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mpv => "mpv",
            Self::Vaapi => "vaapi",
            Self::Software => "software",
            Self::Web => "web",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct VideoInfo {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub duration_secs: f32,
}

/// Script-facing playback state (WE `IVideoTexture`). Time advances while free-running.
#[derive(Debug, Clone)]
pub struct VideoControl {
    pub playing: bool,
    pub looping: bool,
    pub time: f32,
    pub duration: f32,
    /// Playback rate (1.0 = real-time). Applied to wall-clock pacing.
    pub rate: f32,
    /// Bumped when play/pause or rate changes so the decoder re-bases its clock
    /// (prevents catch-up sprint after unpause).
    pub pace_epoch: u64,
}

impl Default for VideoControl {
    fn default() -> Self {
        Self {
            playing: true,
            looping: true,
            time: 0.0,
            duration: 0.0,
            rate: 1.0,
            pace_epoch: 0,
        }
    }
}

enum DecoderInner {
    Mpv(MpvGlDecoder),
    Ffmpeg {
        latest: Arc<Mutex<Option<VideoFrame>>>,
        control: Arc<Mutex<VideoControl>>,
        stop: Arc<AtomicBool>,
        _join: JoinHandle<()>,
        ffmpeg_backend: VideoBackend,
    },
    Web {
        latest: Arc<Mutex<Option<VideoFrame>>>,
        stop: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
    },
}

pub struct VideoDecoder {
    pub path: PathBuf,
    pub backend: VideoBackend,
    pub info: VideoInfo,
    pub out_w: u32,
    pub out_h: u32,
    inner: DecoderInner,
}

impl VideoDecoder {
    pub fn start(path: &Path, max_display_fps: u32) -> Result<Self, String> {
        Self::start_for_display(path, max_display_fps, 3840)
    }

    /// Start a Chromium-backed Wallpaper Engine web project.
    ///
    /// Chromium runs off the Wayland/EGL thread and publishes decoded
    /// screenshots through the same RGBA texture path used by video content.
    pub fn start_web(path: &Path, max_display_fps: u32, max_edge: u32) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("web entrypoint not found: {}", path.display()));
        }
        if which("chromium").is_none() {
            return Err("chromium not found — install Chromium to run web wallpapers".into());
        }
        let max_edge = std::env::var("WALLD_WEB_MAX_EDGE")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(max_edge)
            .clamp(640, 2560);
        // Keep browser snapshots bounded; browser readback is much more
        // expensive than a video frame and the texture is scaled to the
        // output by the existing presentation path.
        let (out_w, out_h) = fit_max_edge(1920, 1080, max_edge);
        let fps = (max_display_fps as f32).clamp(1.0, 30.0);
        let latest = Arc::new(Mutex::new(None));
        let latest2 = latest.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let path_buf = path.canonicalize().map_err(|e| e.to_string())?;
        let join = thread::Builder::new()
            .name("walld-web".into())
            .spawn(move || web_loop(&path_buf, out_w, out_h, fps, &latest2, &stop2))
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now() + Duration::from_secs(20);
        while latest.lock().map(|f| f.is_none()).unwrap_or(true) {
            if join.is_finished() || Instant::now() >= deadline {
                stop.store(true, Ordering::SeqCst);
                let _ = join.join();
                return Err(
                    "Chromium did not produce a web frame; see walld log for the browser error"
                        .into(),
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
        Ok(Self {
            path: path.to_path_buf(),
            backend: VideoBackend::Web,
            info: VideoInfo {
                width: out_w,
                height: out_h,
                fps,
                duration_secs: 0.0,
            },
            out_w,
            out_h,
            inner: DecoderInner::Web {
                latest,
                stop,
                join: Some(join),
            },
        })
    }

    /// Decode for a desktop. `max_edge` = longest edge among monitors.
    /// Prefers libmpv (LWE). `max_display_fps` only caps the ffmpeg fallback
    /// publish rate; mpv presents at source timing.
    pub fn start_for_display(
        path: &Path,
        max_display_fps: u32,
        max_edge: u32,
    ) -> Result<Self, String> {
        if !path.is_file() {
            return Err(format!("video not found: {}", path.display()));
        }

        // LWE path first.
        if video_mpv::prefer_mpv() {
            match MpvGlDecoder::start(path, max_edge) {
                Ok(m) => {
                    return Ok(Self {
                        path: path.to_path_buf(),
                        backend: VideoBackend::Mpv,
                        info: m.info,
                        out_w: m.out_w,
                        out_h: m.out_h,
                        inner: DecoderInner::Mpv(m),
                    });
                }
                Err(e) => {
                    log::warn!("video[mpv] unavailable ({e}); falling back to ffmpeg");
                }
            }
        }

        Self::start_ffmpeg(path, max_display_fps, max_edge)
    }

    fn start_ffmpeg(path: &Path, max_display_fps: u32, max_edge: u32) -> Result<Self, String> {
        if which("ffmpeg").is_none() {
            return Err(
                "ffmpeg not found — install the `ffmpeg` package (in-engine video decoder)".into(),
            );
        }
        let info = probe_video(path).unwrap_or(VideoInfo {
            width: 1920,
            height: 1080,
            fps: 30.0,
            duration_secs: 0.0,
        });
        let max_edge = max_edge.clamp(640, 7680);
        let (out_w, out_h) = fit_max_edge(info.width, info.height, max_edge);
        let source_fps = if info.fps.is_finite() && info.fps >= 1.0 {
            info.fps.clamp(1.0, 120.0)
        } else {
            30.0
        };
        // Prefer source fps; don't force down to a low scene_fps default.
        let publish_fps = source_fps
            .min((max_display_fps as f32).max(source_fps).clamp(5.0, 120.0))
            .max(5.0)
            .min(120.0);

        let latest = Arc::new(Mutex::new(None));
        let latest2 = latest.clone();
        let control = Arc::new(Mutex::new(VideoControl {
            playing: true,
            looping: true,
            time: 0.0,
            duration: info.duration_secs.max(0.0),
            rate: 1.0,
            pace_epoch: 0,
        }));
        let control2 = control.clone();
        let path_buf = path.to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        // VAAPI support is not just a device-level property. AMD's VAAPI
        // decoder rejects H.264 frames above 4096×4096 before the scale_vaapi
        // filter can run. A tiny nullsrc probe therefore gives a false
        // positive for oversized workshop videos (Mokarui is 5760×3240).
        // Choose software up front for sources outside the hardware decoder's
        // input envelope so the worker does not spin on a doomed VAAPI pipe.
        let prefer = match prefer_ffmpeg_backend() {
            VideoBackend::Vaapi if info.width.max(info.height) > 4096 => {
                log::info!(
                    "video source {}×{} exceeds VAAPI 4096px input limit; using software decode",
                    info.width,
                    info.height
                );
                VideoBackend::Software
            }
            other => other,
        };
        let join = thread::Builder::new()
            .name("walld-video".into())
            .spawn(move || {
                let backends: &[VideoBackend] = match prefer {
                    VideoBackend::Vaapi => &[VideoBackend::Vaapi, VideoBackend::Software],
                    _ => &[VideoBackend::Software],
                };
                for &backend in backends {
                    if stop2.load(Ordering::SeqCst) {
                        return;
                    }
                    if let Err(e) = decode_loop(
                        &path_buf,
                        out_w,
                        out_h,
                        source_fps,
                        publish_fps,
                        backend,
                        &latest2,
                        &control2,
                        &stop2,
                    ) {
                        log::warn!("video[{}]: {e}", backend.as_str());
                        if backend == VideoBackend::Vaapi {
                            log::info!("video: falling back to software decode");
                        }
                        continue;
                    }
                    return;
                }
            })
            .map_err(|e| e.to_string())?;

        Ok(Self {
            path: path.to_path_buf(),
            backend: prefer,
            info,
            out_w,
            out_h,
            inner: DecoderInner::Ffmpeg {
                latest,
                control,
                stop,
                _join: join,
                ffmpeg_backend: prefer,
            },
        })
    }

    pub fn start_max_edge(
        path: &Path,
        max_display_fps: u32,
        max_edge: u32,
    ) -> Result<Self, String> {
        Self::start_for_display(path, max_display_fps, max_edge)
    }

    pub fn is_mpv(&self) -> bool {
        matches!(self.inner, DecoderInner::Mpv(_))
    }

    /// Source fps for present pacing (mpv or probed).
    pub fn source_fps(&self) -> f32 {
        if self.info.fps.is_finite() && self.info.fps >= 1.0 {
            self.info.fps.clamp(1.0, 120.0)
        } else {
            30.0
        }
    }

    pub fn try_frame(&self) -> Option<VideoFrame> {
        match &self.inner {
            DecoderInner::Ffmpeg { latest, .. } => latest.lock().ok().and_then(|mut g| g.take()),
            DecoderInner::Web { latest, .. } => latest.lock().ok().and_then(|mut g| g.take()),
            DecoderInner::Mpv(_) => None,
        }
    }

    /// LWE path: render current frame into a GL texture (requires current context).
    /// Returns `None` while the first frame is still buffering — keep any poster.
    pub fn render_mpv(&mut self, gl: &glow::Context) -> Option<(glow::Texture, u32, u32)> {
        match &mut self.inner {
            DecoderInner::Mpv(m) => match m.render(gl) {
                Ok(t) => Some(t),
                Err(e) => {
                    // "not ready" is expected for the first few ticks; don't spam.
                    if !e.contains("not ready") {
                        log::warn!("video[mpv] render: {e}");
                    }
                    None
                }
            },
            DecoderInner::Ffmpeg { .. } => None,
            DecoderInner::Web { .. } => None,
        }
    }

    pub fn destroy_mpv_gl(&mut self, gl: &glow::Context) {
        if let DecoderInner::Mpv(m) = &mut self.inner {
            m.destroy_gl(gl);
        }
    }

    pub fn control_snapshot(&self) -> VideoControl {
        match &self.inner {
            DecoderInner::Mpv(m) => m.control_snapshot(),
            DecoderInner::Ffmpeg { control, .. } => {
                control.lock().map(|g| g.clone()).unwrap_or_default()
            }
            DecoderInner::Web { .. } => VideoControl::default(),
        }
    }

    /// Scripts may request pause/seek. Mpv honors seek; ffmpeg pipe ignores seek.
    /// Default free-run unless WALLD_VIDEO_SCRIPT_CONTROL=1.
    pub fn apply_script_control(&self, playing: bool, looping: bool, seek_to: Option<f32>) {
        match &self.inner {
            DecoderInner::Mpv(m) => m.apply_script_control(playing, looping, seek_to),
            DecoderInner::Ffmpeg { control, .. } => {
                let honor = std::env::var("WALLD_VIDEO_SCRIPT_CONTROL")
                    .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                if !honor {
                    return;
                }
                if let Ok(mut g) = control.lock() {
                    g.playing = playing;
                    g.looping = looping;
                }
            }
            DecoderInner::Web { .. } => {}
        }
    }

    pub fn set_user_playing(&self, playing: bool) {
        match &self.inner {
            DecoderInner::Mpv(m) => m.set_user_playing(playing),
            DecoderInner::Ffmpeg { control, .. } => {
                if let Ok(mut g) = control.lock() {
                    if g.playing != playing {
                        g.playing = playing;
                        g.pace_epoch = g.pace_epoch.wrapping_add(1);
                    }
                }
            }
            DecoderInner::Web { .. } => {}
        }
    }

    pub fn set_rate(&self, rate: f32) {
        match &self.inner {
            DecoderInner::Mpv(m) => m.set_rate(rate),
            DecoderInner::Ffmpeg { control, .. } => {
                let rate = rate.clamp(0.05, 4.0);
                if let Ok(mut g) = control.lock() {
                    if (g.rate - rate).abs() > 0.0001 {
                        g.rate = rate;
                        g.pace_epoch = g.pace_epoch.wrapping_add(1);
                    }
                }
            }
            DecoderInner::Web { .. } => {}
        }
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        if let DecoderInner::Ffmpeg { stop, .. } = &self.inner {
            stop.store(true, Ordering::SeqCst);
        }
        if let DecoderInner::Web { stop, join, .. } = &mut self.inner {
            stop.store(true, Ordering::SeqCst);
            if let Some(join) = join.take() {
                let _ = join.join();
            }
        }
    }
}

fn prefer_ffmpeg_backend() -> VideoBackend {
    if let Ok(v) = std::env::var("WALLD_HWACCEL") {
        let v = v.to_ascii_lowercase();
        if matches!(
            v.as_str(),
            "0" | "off" | "false" | "software" | "sw" | "none"
        ) {
            return VideoBackend::Software;
        }
    }
    if vaapi_device().is_some() {
        VideoBackend::Vaapi
    } else {
        VideoBackend::Software
    }
}

fn vaapi_device() -> Option<PathBuf> {
    if let Ok(d) = std::env::var("WALLD_VAAPI_DEVICE") {
        let p = PathBuf::from(d);
        if p.exists() {
            return Some(p);
        }
    }
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev/dri")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("renderD"))
                .unwrap_or(false)
        })
        .collect();
    nodes.sort();
    for n in &nodes {
        if probe_vaapi(n) {
            return Some(n.clone());
        }
    }
    None
}

fn probe_vaapi(device: &Path) -> bool {
    let status = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-hwaccel",
            "vaapi",
            "-hwaccel_device",
        ])
        .arg(device)
        .args([
            "-f",
            "lavfi",
            "-i",
            "nullsrc=s=64x64:d=0.05",
            "-frames:v",
            "1",
            "-f",
            "null",
            "-",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    matches!(status, Ok(s) if s.success())
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn decode_loop(
    path: &Path,
    out_w: u32,
    out_h: u32,
    source_fps: f32,
    publish_fps: f32,
    backend: VideoBackend,
    latest: &Mutex<Option<VideoFrame>>,
    control: &Mutex<VideoControl>,
    stop: &AtomicBool,
) -> Result<(), String> {
    // One long-running stream with ffmpeg -stream_loop -1. Only restart on error.
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match decode_file(
            path,
            out_w,
            out_h,
            source_fps,
            publish_fps,
            backend,
            latest,
            control,
            stop,
        ) {
            Ok(()) if stop.load(Ordering::SeqCst) => return Ok(()),
            Ok(()) => {
                // Unexpected end without stop — brief pause then reopen.
                log::debug!("video: stream ended, reopening");
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                log::warn!("video[{}]: {e}", backend.as_str());
                return Err(e);
            }
        }
    }
}

/// Render a web wallpaper by asking Chromium for periodic headless snapshots.
///
/// This deliberately lives outside the Wayland thread. A browser compositor
/// cannot safely share walld's EGL context, while the resulting RGBA frame can
/// use the same upload/presentation path as any other wallpaper source.
fn web_loop(
    path: &Path,
    out_w: u32,
    out_h: u32,
    fps: f32,
    latest: &Mutex<Option<VideoFrame>>,
    stop: &AtomicBool,
) {
    let interval = Duration::from_secs_f32(1.0 / fps.max(1.0));
    let Ok(profile_dir) = tempfile::Builder::new().prefix("walld-web-").tempdir() else {
        log::error!("cannot create web browser profile");
        return;
    };
    let profile = profile_dir.path();
    let uri = url::Url::from_file_path(path)
        .expect("canonical web entrypoint")
        .to_string();
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            log::warn!("web wallpaper could not reserve a DevTools port: {e}");
            return;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    drop(listener);
    let mut browser = match Command::new("chromium")
        .args([
            "--headless=new",
            "--disable-dev-shm-usage",
            "--disable-extensions",
            "--no-first-run",
            "--no-default-browser-check",
            "--allow-file-access-from-files",
            "--hide-scrollbars",
            "--run-all-compositor-stages-before-draw",
            "--force-device-scale-factor=1",
        ])
        .arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--window-size={out_w},{out_h}"))
        .arg("about:blank")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            log::warn!("web wallpaper Chromium failed to start: {e}");
            return;
        }
    };
    // WebGL pages may take several seconds to initialize before Chromium
    // publishes the target, especially on the first run with a fresh profile.
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut ws_url = None;
    while !stop.load(Ordering::SeqCst) && Instant::now() < deadline {
        if browser.try_wait().ok().flatten().is_some() {
            break;
        }
        let found = cdp_ws_url(port);
        if found.is_none() {
            thread::sleep(Duration::from_millis(50));
        }
        if found.is_some() {
            ws_url = found;
            break;
        }
    }
    let Some(ws_url) = ws_url else {
        log::warn!("web wallpaper Chromium did not expose a DevTools target");
        let _ = browser.kill();
        let _ = browser.wait();
        let _ = std::fs::remove_dir_all(profile);
        return;
    };
    let mut socket = match ws_connect(&ws_url, port) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("web wallpaper DevTools connection failed: {e}");
            let _ = browser.kill();
            let _ = browser.wait();
            let _ = std::fs::remove_dir_all(profile);
            return;
        }
    };
    let mut request_id = 1u64;
    let setup = (|| -> Result<(), String> {
        cdp_command(
            &mut socket,
            &mut request_id,
            "Page.enable",
            serde_json::json!({}),
        )?;
        cdp_command(
            &mut socket,
            &mut request_id,
            "Emulation.setDeviceMetricsOverride",
            serde_json::json!({
                "width": out_w,
                "height": out_h,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
        )?;
        let parent = path.parent().ok_or("missing project directory")?;
        let dir = parent
            .ancestors()
            .find(|dir| dir.join("project.json").is_file())
            .unwrap_or(parent);
        let id = dir.file_name().unwrap_or_default().to_string_lossy();
        let properties = wallengine_we::props::load_merged_properties(dir, &id);
        let properties = serde_json::to_string(&properties).map_err(|e| e.to_string())?;
        let bridge = format!(
            r#"
        (() => {{
            const properties = {properties};
            // Audio capture is not available in this backend yet. Supply
            // silence so packages can initialize without a missing API error.
            window.wallpaperRegisterAudioListener = callback => {{
                setInterval(() => callback(new Array(128).fill(0)), 100);
            }};
            window.wallpaperRegisterMediaPropertiesListener = () => {{}};
            window.wallpaperRegisterMediaPlaybackListener = () => {{}};
            window.wallpaperRegisterMediaTimelineListener = () => {{}};
            window.wallpaperRegisterMediaThumbnailListener = () => {{}};
            window.addEventListener('load', () => {{
                const listener = window.wallpaperPropertyListener;
                listener?.applyGeneralProperties?.({{fps: {fps}}});
                listener?.applyUserProperties?.(properties);
            }});
        }})();
    "#
        );
        cdp_command(
            &mut socket,
            &mut request_id,
            "Page.addScriptToEvaluateOnNewDocument",
            serde_json::json!({"source": bridge}),
        )?;
        let result = cdp_command(
            &mut socket,
            &mut request_id,
            "Page.navigate",
            serde_json::json!({"url": uri}),
        )?;
        if let Some(error) = result.get("errorText") {
            return Err(error.to_string());
        }
        Ok(())
    })();
    if let Err(error) = setup {
        log::error!("web wallpaper setup failed: {error}");
        let _ = browser.kill();
        let _ = browser.wait();
        return;
    }
    log::info!("web wallpaper Chromium renderer ready {}×{}", out_w, out_h);
    let mut failures = 0;
    let document_deadline = Instant::now() + Duration::from_secs(15);
    while !stop.load(Ordering::SeqCst) && Instant::now() < document_deadline {
        let state = cdp_command(
            &mut socket,
            &mut request_id,
            "Runtime.evaluate",
            serde_json::json!({"expression": "document.readyState", "returnByValue": true}),
        );
        if state.ok().and_then(|s| {
            s.pointer("/result/value")
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        }) == Some("complete".into())
        {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    while !stop.load(Ordering::SeqCst) {
        let started = Instant::now();
        match cdp_command(
            &mut socket,
            &mut request_id,
            "Page.captureScreenshot",
            serde_json::json!({ "format": "png", "fromSurface": true }),
        ) {
            Ok(result) => {
                failures = 0;
                let decoded = result
                    .get("data")
                    .and_then(|v| v.as_str())
                    .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
                    .and_then(|bytes| image::load_from_memory(&bytes).ok());
                if let Some(img) = decoded {
                    let rgba = img.to_rgba8();
                    if let Ok(mut slot) = latest.lock() {
                        *slot = Some(VideoFrame {
                            width: rgba.width(),
                            height: rgba.height(),
                            rgba: rgba.into_raw(),
                        });
                    }
                } else {
                    log::warn!("web wallpaper Chromium returned an invalid screenshot");
                }
            }
            Err(e) => {
                failures += 1;
                // The target briefly has no active document during navigation.
                if failures <= 20 {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                log::warn!("web wallpaper screenshot failed: {e}");
                break;
            }
        }
        let elapsed = started.elapsed();
        if elapsed < interval {
            thread::sleep(interval - elapsed);
        }
    }
    let _ = browser.kill();
    let _ = browser.wait();
    let _ = std::fs::remove_dir_all(profile);
}

fn cdp_ws_url(port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    stream
        .write_all(
            format!(
                "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .ok()?;
    let mut bytes = Vec::new();
    // Chromium advertises Content-Length but may keep the HTTP connection
    // alive, so waiting for EOF would stall target discovery indefinitely.
    let mut buf = [0u8; 4096];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                break
            }
            Err(_) => return None,
        }
    }
    let body = std::str::from_utf8(&bytes).ok()?.split("\r\n\r\n").nth(1)?;
    let targets: serde_json::Value = serde_json::from_str(body).ok()?;
    targets
        .as_array()?
        .iter()
        .filter(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
        .find_map(|t| t.get("webSocketDebuggerUrl").and_then(|v| v.as_str()))
        .map(str::to_owned)
}

type WebSocket = tungstenite::WebSocket<TcpStream>;

fn ws_connect(endpoint: &str, port: u16) -> Result<WebSocket, String> {
    let mut url = url::Url::parse(endpoint).map_err(|e| e.to_string())?;
    url.set_host(Some("127.0.0.1")).map_err(|e| e.to_string())?;
    url.set_port(Some(port))
        .map_err(|_| "invalid DevTools port")?;
    let stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    tungstenite::client(url.as_str(), stream)
        .map(|(socket, _)| socket)
        .map_err(|e| e.to_string())
}

fn cdp_command(
    socket: &mut WebSocket,
    request_id: &mut u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let id = *request_id;
    *request_id = request_id.wrapping_add(1);
    let message = serde_json::json!({ "id": id, "method": method, "params": params }).to_string();
    socket
        .send(tungstenite::Message::Text(message.into()))
        .map_err(|e| e.to_string())?;
    loop {
        let message = socket.read().map_err(|e| e.to_string())?;
        if message.is_close() {
            return Err("Chromium disconnected".into());
        }
        let tungstenite::Message::Text(text) = message else {
            continue;
        };
        let value: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        if value.get("id").and_then(|v| v.as_u64()) != Some(id) {
            continue;
        }
        if let Some(error) = value.get("error") {
            return Err(error.to_string());
        }
        return Ok(value
            .get("result")
            .cloned()
            .unwrap_or(serde_json::Value::Null));
    }
}

fn decode_file(
    path: &Path,
    out_w: u32,
    out_h: u32,
    source_fps: f32,
    publish_fps: f32,
    backend: VideoBackend,
    latest: &Mutex<Option<VideoFrame>>,
    control: &Mutex<VideoControl>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut child = spawn_ffmpeg(path, out_w, out_h, backend)?;
    log::info!(
        "video decode {} {}×{} via {} (source {:.3} fps, publish ≤{:.1} fps)",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("?"),
        out_w,
        out_h,
        backend.as_str(),
        source_fps,
        publish_fps,
    );

    let stdout = child.stdout.take().ok_or("ffmpeg stdout")?;
    let nbytes = out_w as usize * out_h as usize * 4;
    let mut reader = BufReader::with_capacity(nbytes.min(4 << 20), stdout);
    let mut buf = vec![0u8; nbytes];

    let mut frame_idx: u64 = 0;
    let publish_every = (source_fps / publish_fps).round().max(1.0) as u64;
    let duration = control.lock().map(|g| g.duration).unwrap_or(0.0);

    // Pace from a re-baseable clock so pause / rate changes don't cause catch-up.
    let mut pace_epoch: u64 = 0;
    let mut pace_start = Instant::now();
    let mut media_at_pace_start: f64 = 0.0;
    let mut media_t_abs: f64 = 0.0; // absolute media seconds (monotonic along file)

    while !stop.load(Ordering::SeqCst) {
        let (playing, rate, epoch) = control
            .lock()
            .map(|g| (g.playing, g.rate.max(0.05), g.pace_epoch))
            .unwrap_or((true, 1.0, 0));
        if !playing {
            thread::sleep(Duration::from_millis(16));
            continue;
        }
        if epoch != pace_epoch {
            // Unpaused or rate changed: continue from current media time in real time.
            pace_epoch = epoch;
            pace_start = Instant::now();
            media_at_pace_start = media_t_abs;
        }

        if let Err(e) = reader.read_exact(&mut buf) {
            // With -stream_loop -1 a clean EOF is not expected. Treat a
            // short pipe as a backend failure so VAAPI can fall back instead
            // of reopening the same broken process forever.
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("decoder pipe ended before a complete frame: {e}"));
        }
        frame_idx += 1;
        media_t_abs = frame_idx as f64 / source_fps as f64;
        let media_t = if duration > 0.0 {
            // Loop media time into [0, duration) for scripts reading getCurrentTime().
            (media_t_abs as f32) % duration
        } else {
            media_t_abs as f32
        };
        if let Ok(mut g) = control.lock() {
            g.time = media_t;
        }

        // Wall-clock due: media advanced since last rebase, scaled by rate.
        // rate > 1 → shorter waits (faster); rate < 1 → longer waits.
        let media_delta = (media_t_abs - media_at_pace_start).max(0.0);
        let wall_delta = media_delta / rate as f64;
        let due = pace_start + Duration::from_secs_f64(wall_delta);
        while !stop.load(Ordering::SeqCst) {
            // Honor mid-wait pause: don't burn CPU racing after unpause.
            let still_playing = control.lock().map(|g| g.playing).unwrap_or(true);
            if !still_playing {
                break;
            }
            let now = Instant::now();
            if now >= due {
                break;
            }
            thread::sleep((due - now).min(Duration::from_millis(5)));
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // If we broke out for pause, don't publish this frame as "live" catch-up.
        if !control.lock().map(|g| g.playing).unwrap_or(true) {
            continue;
        }

        if publish_every > 1 && (frame_idx % publish_every) != 0 {
            continue;
        }

        if let Ok(mut g) = latest.lock() {
            let recycled = g.take().map(|f| f.rgba);
            let frame = VideoFrame {
                width: out_w,
                height: out_h,
                rgba: std::mem::take(&mut buf),
            };
            *g = Some(frame);
            buf = match recycled {
                Some(v) if v.len() == nbytes => v,
                Some(mut v) => {
                    v.resize(nbytes, 0);
                    v
                }
                None => vec![0u8; nbytes],
            };
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn spawn_ffmpeg(path: &Path, w: u32, h: u32, backend: VideoBackend) -> Result<Child, String> {
    let path_s = path.to_string_lossy();
    match backend {
        VideoBackend::Mpv => Err("ffmpeg spawn called with Mpv backend".into()),
        VideoBackend::Web => Err("ffmpeg spawn called with Web backend".into()),
        VideoBackend::Vaapi => {
            let device = vaapi_device().ok_or_else(|| "no VAAPI device".to_string())?;
            Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-hwaccel",
                    "vaapi",
                    "-hwaccel_device",
                ])
                .arg(&device)
                .args([
                    "-hwaccel_output_format",
                    "vaapi",
                    "-stream_loop",
                    "-1",
                    "-i",
                ])
                .arg(path_s.as_ref())
                .args([
                    "-an",
                    "-vf",
                    &format!("scale_vaapi=w={w}:h={h},hwdownload,format=rgba"),
                    "-f",
                    "rawvideo",
                    "-pix_fmt",
                    "rgba",
                    "pipe:1",
                ])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|e| format!("ffmpeg vaapi: {e}"))
        }
        VideoBackend::Software => Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-stream_loop",
                "-1",
                "-i",
            ])
            .arg(path_s.as_ref())
            .args([
                "-an",
                "-vf",
                &format!("scale={w}:{h}:flags=bicubic"),
                "-f",
                "rawvideo",
                "-pix_fmt",
                "rgba",
                "pipe:1",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("ffmpeg: {e}")),
    }
}

pub fn probe_video(path: &Path) -> Option<VideoInfo> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height,avg_frame_rate,r_frame_rate,duration",
            "-show_entries",
            "format=duration",
            "-of",
            "json",
            &path.to_string_lossy(),
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let stream = v.get("streams")?.as_array()?.first()?;
    let width = stream.get("width")?.as_u64()? as u32;
    let height = stream.get("height")?.as_u64()? as u32;
    let fps = parse_rate(stream.get("avg_frame_rate").and_then(|x| x.as_str()))
        .or_else(|| parse_rate(stream.get("r_frame_rate").and_then(|x| x.as_str())))
        .unwrap_or(30.0);
    let duration_secs = stream
        .get("duration")
        .and_then(|x| x.as_str())
        .and_then(|s| s.parse().ok())
        .or_else(|| {
            v.get("format")
                .and_then(|f| f.get("duration"))
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse().ok())
        })
        .unwrap_or(0.0);
    Some(VideoInfo {
        width,
        height,
        fps,
        duration_secs,
    })
}

fn parse_rate(s: Option<&str>) -> Option<f32> {
    let s = s?;
    if s == "0/0" || s.is_empty() {
        return None;
    }
    if let Some((a, b)) = s.split_once('/') {
        let num: f32 = a.parse().ok()?;
        let den: f32 = b.parse().ok()?;
        if den > 0.0 {
            return Some(num / den);
        }
    }
    s.parse().ok()
}

fn fit_max_edge(w: u32, h: u32, max_edge: u32) -> (u32, u32) {
    let w = w.max(1);
    let h = h.max(1);
    let long = w.max(h);
    if long <= max_edge {
        return (w & !1, h & !1);
    }
    let scale = max_edge as f32 / long as f32;
    let nw = ((w as f32 * scale).round() as u32).max(2) & !1;
    let nh = ((h as f32 * scale).round() as u32).max(2) & !1;
    (nw, nh)
}

pub fn hw_decode_available() -> bool {
    prefer_ffmpeg_backend() == VideoBackend::Vaapi
}
