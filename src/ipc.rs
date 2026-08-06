//! Unix-socket IPC: server side runs in the daemon, `walld ctl <cmd>` is the client.
//!
//! Protocol: one text line in, one line out ("ok ..." / "err ...").

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use calloop::generic::Generic;
use calloop::{Interest, LoopHandle, Mode, PostAction};

pub fn sock_path() -> PathBuf {
    let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(run).join("walld.sock")
}

#[derive(Clone, Debug)]
pub enum IpcCmd {
    Ping,
    Status,
    Reload,
    /// monitor == "*" means all outputs.
    Set { monitor: String, path: PathBuf },
    Snap { monitor: String, path: PathBuf },
    Wipe { monitor: String, path: PathBuf },
    Preload { path: PathBuf },
    Stop,
    Start,
    Ready,
    /// Load a scene document on monitor(s).
    Scene { monitor: String, path: PathBuf },
    Quit,
}

fn expand(path: &str) -> PathBuf {
    let p = path.trim();
    if let Some(rest) = p.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

pub fn parse_line(line: &str) -> Result<IpcCmd, String> {
    let mut parts = line.split_whitespace();
    let cmd = parts.next().ok_or("empty command")?;
    let next = |parts: &mut std::str::SplitWhitespace, what: &str| -> Result<String, String> {
        parts.next().map(|s| s.to_string()).ok_or_else(|| format!("missing {what}"))
    };
    Ok(match cmd {
        "ping" => IpcCmd::Ping,
        "status" => IpcCmd::Status,
        "reload" => IpcCmd::Reload,
        "set" | "snap" | "wipe" => {
            let monitor = next(&mut parts, "<monitor|*>")?;
            let path = expand(&next(&mut parts, "<path>")?);
            match cmd {
                "set" => IpcCmd::Set { monitor, path },
                "snap" => IpcCmd::Snap { monitor, path },
                _ => IpcCmd::Wipe { monitor, path },
            }
        }
        "preload" => IpcCmd::Preload { path: expand(&next(&mut parts, "<path>")?) },
        "stop" => IpcCmd::Stop,
        "start" => IpcCmd::Start,
        "ready" => IpcCmd::Ready,
        "scene" => {
            let monitor = next(&mut parts, "<monitor|*>")?;
            let path = expand(&next(&mut parts, "<path>")?);
            IpcCmd::Scene { monitor, path }
        }
        "quit" => IpcCmd::Quit,
        other => return Err(format!("unknown command '{other}'")),
    })
}

pub struct IpcServer;

impl IpcServer {
    /// Bind the socket and register the listener with the event loop.
    /// Each accepted connection is read synchronously for one command line
    /// (clients are tiny CLI invocations) and dispatched to the daemon.
    pub fn start<D: 'static>(
        handle: &LoopHandle<'_, D>,
        dispatch: impl FnMut(&mut D, (UnixStream, IpcCmd)) + 'static,
    ) -> std::io::Result<Self> {
        let path = sock_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        // Stale socket from a crashed run: only safe to remove if nothing answers.
        if UnixStream::connect(&path).is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AddrInUse,
                format!("walld already running ({} answers)", path.display()),
            ));
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)?;

        let mut dispatch = dispatch;
        let source = Generic::new(listener, Interest::READ, Mode::Level);
        handle
            .insert_source(source, move |_readiness, io_obj, state| {
                let (stream, _) = match io_obj.accept() {
                    Ok(x) => x,
                    Err(e) => {
                        log::warn!("ipc accept: {e}");
                        return Ok(PostAction::Continue);
                    }
                };
                let mut line = String::new();
                let read_ok = BufReader::new(&stream).read_line(&mut line).is_ok();
                if !read_ok {
                    let mut s = stream;
                    let _ = s.write_all(b"err read\n");
                    return Ok(PostAction::Continue);
                }
                match parse_line(line.trim()) {
                    Ok(cmd) => dispatch(state, (stream, cmd)),
                    Err(e) => {
                        let mut s = stream;
                        let _ = s.write_all(format!("err {e}\n").as_bytes());
                    }
                }
                Ok(PostAction::Continue)
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        Ok(IpcServer)
    }
}

/// Client side: send one command, print the reply, return process exit code.
pub fn client_call(line: &str) -> i32 {
    let path = sock_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("walld: cannot reach daemon at {} ({e}) — is walld running?", path.display());
            return 1;
        }
    };
    if stream.write_all(line.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
        eprintln!("walld: write failed");
        return 1;
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let mut resp = String::new();
    let _ = stream.read_to_string(&mut resp);
    print!("{resp}");
    if !resp.ends_with('\n') {
        println!();
    }
    if resp.starts_with("ok") {
        0
    } else {
        1
    }
}
