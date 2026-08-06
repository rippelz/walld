//! In-engine video wallpaper decoder.
//! Uses the system `ffmpeg` binary as a codec backend (same as linking libav),
//! owned entirely by walld — not a separate wallpaper process like mpvpaper.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct VideoDecoder {
    pub path: PathBuf,
    rx: Receiver<VideoFrame>,
    stop: Arc<AtomicBool>,
    _join: JoinHandle<()>,
}

impl VideoDecoder {
    pub fn start(path: &Path, target_fps: u32) -> Result<Self, String> {
        if which("ffmpeg").is_none() {
            return Err(
                "ffmpeg not found — install the `ffmpeg` package (in-engine video decoder)".into(),
            );
        }
        if !path.is_file() {
            return Err(format!("video not found: {}", path.display()));
        }
        let (tx, rx) = mpsc::sync_channel::<VideoFrame>(2);
        let path_buf = path.to_path_buf();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = stop.clone();
        let fps = target_fps.clamp(5, 60);
        let join = thread::Builder::new()
            .name("walld-video".into())
            .spawn(move || {
                while !stop2.load(Ordering::SeqCst) {
                    if let Err(e) = decode_file(&path_buf, fps, &tx, &stop2) {
                        log::warn!("video: {e}");
                        thread::sleep(Duration::from_millis(400));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self {
            path: path.to_path_buf(),
            rx,
            stop,
            _join: join,
        })
    }

    pub fn try_frame(&self) -> Option<VideoFrame> {
        let mut last = None;
        loop {
            match self.rx.try_recv() {
                Ok(f) => last = Some(f),
                Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => break,
            }
        }
        last
    }
}

impl Drop for VideoDecoder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
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

fn decode_file(
    path: &Path,
    fps: u32,
    tx: &SyncSender<VideoFrame>,
    stop: &AtomicBool,
) -> Result<(), String> {
    let (w, h) = probe_size(path).unwrap_or((1920, 1080));
    let w = w.min(3840);
    let h = h.min(2160);
    let mut child: Child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-stream_loop",
            "-1",
            "-i",
            &path.to_string_lossy(),
            "-vf",
            &format!("fps={fps},scale={w}:{h}:flags=fast_bilinear"),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("ffmpeg: {e}"))?;

    let stdout = child.stdout.take().ok_or("ffmpeg stdout")?;
    let mut reader = BufReader::with_capacity(w as usize * h as usize * 4, stdout);
    let nbytes = w as usize * h as usize * 4;
    let mut buf = vec![0u8; nbytes];
    let frame_dt = Duration::from_secs_f32(1.0 / fps as f32);
    let mut next = Instant::now();

    while !stop.load(Ordering::SeqCst) {
        if let Err(e) = reader.read_exact(&mut buf) {
            log::debug!("video eof/err: {e}");
            break;
        }
        let frame = VideoFrame {
            width: w,
            height: h,
            rgba: buf.clone(),
        };
        match tx.try_send(frame) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                // consumer still has a frame; drop this one
            }
            Err(mpsc::TrySendError::Disconnected(_)) => break,
        }
        let now = Instant::now();
        if next > now {
            thread::sleep(next - now);
        }
        next = Instant::now() + frame_dt;
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(())
}

fn probe_size(path: &Path) -> Option<(u32, u32)> {
    let out = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=p=0:s=x",
            &path.to_string_lossy(),
        ])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    let mut it = s.trim().split('x');
    let w = it.next()?.parse().ok()?;
    let h = it.next()?.parse().ok()?;
    Some((w, h))
}
