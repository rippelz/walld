//! A tiny per-user Unix-socket handoff for WallStudio.
//!
//! Desktop launchers start a fresh process every time.  The first process owns
//! this socket; later launches send it `raise` and immediately exit, letting
//! the running app bring its existing library window to the foreground.

use iced::futures::channel::mpsc::{self, UnboundedReceiver, UnboundedSender};
use iced::futures::stream;
use iced::futures::StreamExt;
use iced::Subscription;
use std::fs;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const RAISE: &[u8] = b"raise\n";

/// Ownership of the socket. Dropping the primary process removes its endpoint
/// so a future launch is never mistaken for an already-open window.
pub struct Guard {
    path: PathBuf,
}

impl Drop for Guard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The successfully acquired single-instance endpoint and its event stream.
pub struct Primary {
    pub guard: Guard,
    pub stream: Stream,
}

/// Try to become the primary WallStudio process. `Ok(None)` means a primary
/// process accepted a raise request, so the caller must exit without opening a
/// second window.
pub fn acquire_or_raise() -> io::Result<Option<Primary>> {
    let path = socket_path();
    if raise_existing(&path) {
        return Ok(None);
    }

    // A dead process can leave its socket pathname behind. Only remove an
    // actual Unix socket; never unlink an unexpected user file at this path.
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to replace non-socket {}", path.display()),
            ));
        }
        fs::remove_file(&path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) if err.kind() == io::ErrorKind::AddrInUse && raise_existing(&path) => {
            return Ok(None);
        }
        Err(err) => return Err(err),
    };
    listener.set_nonblocking(true)?;

    let (tx, rx) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("wallstudio-instance".into())
        .spawn(move || serve(listener, tx))?;

    Ok(Some(Primary {
        guard: Guard { path },
        stream: Stream::new(rx),
    }))
}

fn socket_path() -> PathBuf {
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(runtime).join("wallstudio.sock");
    }
    // XDG_RUNTIME_DIR is normally present under a graphical login. This
    // fallback stays per-user, unlike a shared /tmp/wallstudio.sock.
    let user = std::env::var("UID")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into());
    let user: String = user
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    PathBuf::from("/tmp").join(format!("wallstudio-{user}.sock"))
}

fn raise_existing(path: &PathBuf) -> bool {
    UnixStream::connect(path)
        .and_then(|mut stream| stream.write_all(RAISE))
        .is_ok()
}

fn serve(listener: UnixListener, tx: UnboundedSender<()>) {
    loop {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut request = [0_u8; 32];
                if let Ok(len) = stream.read(&mut request) {
                    if request[..len].starts_with(b"raise") {
                        let _ = tx.unbounded_send(());
                    }
                }
            }
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => break,
        }
    }
}

/// Hashable receiver wrapper so Iced preserves the subscription across redraws.
#[derive(Clone)]
pub struct Stream {
    rx: Arc<Mutex<Option<UnboundedReceiver<()>>>>,
}

impl Stream {
    fn new(rx: UnboundedReceiver<()>) -> Self {
        Self {
            rx: Arc::new(Mutex::new(Some(rx))),
        }
    }
}

impl Hash for Stream {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "wallstudio::single-instance".hash(state);
    }
}

pub fn subscription(stream: Stream) -> Subscription<()> {
    Subscription::run_with(stream, |stream| {
        let inner = stream.rx.clone();
        stream::unfold(
            inner,
            |inner: Arc<Mutex<Option<UnboundedReceiver<()>>>>| async move {
                let mut rx = inner.lock().unwrap().take()?;
                rx.next().await?;
                let seed = Arc::new(Mutex::new(Some(rx)));
                Some(((), seed))
            },
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raise_request_reaches_the_primary_socket() {
        let path = std::env::temp_dir().join(format!(
            "wallstudio-instance-test-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 16];
            let len = stream.read(&mut request).unwrap();
            request[..len].to_vec()
        });
        assert!(raise_existing(&path));
        assert_eq!(reader.join().unwrap(), RAISE.to_vec());
        let _ = fs::remove_file(path);
    }
}
