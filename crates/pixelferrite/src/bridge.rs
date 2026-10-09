//! A private, per-user local socket for the MCP server. Commands are handed to
//! the UI thread, with bounded messages, connections, queued work and waits.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}, mpsc::{Receiver, Sender, SyncSender, channel, sync_channel}};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub(crate) const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_REPLY_BYTES: usize = 32 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 8;
const MAX_QUEUED: usize = 16;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
const IO_TIMEOUT: Duration = Duration::from_secs(5);

type Reply = Sender<Result<Value, String>>;
struct Pending {
    cmd: Value,
    reply: Reply,
    expires: Instant,
    cancelled: Arc<AtomicBool>,
}

pub struct Bridge {
    rx: Receiver<Pending>,
    stop: Arc<AtomicBool>,
    ctx: egui::Context,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

impl Bridge {
    /// Listens in a mode-0700 directory owned by the current user. A second
    /// window cannot steal an active socket. Unsupported platforms fail closed.
    pub fn start(port: u16, ctx: egui::Context) -> Option<Bridge> {
        #[cfg(unix)]
        {
            match unix::start(port, ctx) {
                Ok(bridge) => Some(bridge),
                Err(e) => { eprintln!("MCP bridge unavailable: {e}"); None }
            }
        }
        #[cfg(not(unix))]
        { let _ = (port, ctx); None }
    }

    /// Limit work per frame and discard commands whose callers already timed out.
    pub fn serve(&self, mut run: impl FnMut(&Value) -> Result<Value, String>) {
        for _ in 0..4 {
            let Ok(pending) = self.rx.try_recv() else { return };
            if pending.cancelled.load(Ordering::Acquire) || Instant::now() >= pending.expires {
                let _ = pending.reply.send(Err("Command expired before execution".to_owned()));
                continue;
            }
            let _ = pending.reply.send(run(&pending.cmd));
        }
        self.ctx.request_repaint();
    }
}

/// Read a complete, bounded line. EOF in the middle of a frame is an error.
/// The limit is checked while reading, not after allocating an unlimited line.
pub(crate) fn read_frame(reader: &mut impl BufRead, limit: usize) -> io::Result<Option<Vec<u8>>> {
    read_frame_inner(reader, limit, None)
}

fn read_frame_inner(reader: &mut impl BufRead, limit: usize, deadline: Option<Instant>) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "JSON frame deadline expired"));
        }
        let bytes = reader.fill_buf()?;
        if bytes.is_empty() {
            return if line.is_empty() { Ok(None) } else { Err(io::Error::new(io::ErrorKind::UnexpectedEof, "incomplete JSON frame")) };
        }
        let end = bytes.iter().position(|&b| b == b'\n');
        let count = end.map_or(bytes.len(), |n| n + 1);
        if line.len().saturating_add(count) > limit {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "JSON frame exceeds size limit"));
        }
        line.extend_from_slice(&bytes[..count]);
        reader.consume(count);
        if end.is_some() { return Ok(Some(line)); }
    }
}

fn read_json(reader: &mut impl BufRead, limit: usize, timeout: Duration) -> io::Result<Value> {
    // Socket reads also have an inactivity timeout. An absolute frame deadline
    // stops trickle traffic from holding a connection forever.
    let frame = read_frame_inner(reader, limit, Some(Instant::now() + timeout))?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed the connection"))?;
    serde_json::from_slice(&frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

fn write_json(writer: &mut impl Write, value: &Value, limit: usize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() > limit { return Err(io::Error::new(io::ErrorKind::InvalidInput, "JSON frame exceeds size limit")); }
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut remaining = bytes.as_slice();
    while !remaining.is_empty() {
        if Instant::now() >= deadline { return Err(io::Error::new(io::ErrorKind::TimedOut, "JSON write deadline expired")); }
        let n = writer.write(remaining)?;
        if n == 0 { return Err(io::Error::new(io::ErrorKind::WriteZero, "peer stopped accepting JSON")); }
        remaining = &remaining[n..];
    }
    writer.flush()
}

#[derive(Debug)]
pub(crate) struct CallReply {
    pub instance: String,
    pub result: Result<Value, String>,
}

#[derive(Debug)]
pub(crate) struct CallError {
    pub source: io::Error,
    pub connected: bool,
    pub instance: Option<String>,
    pub may_have_executed: bool,
}

impl CallError {
    pub fn app_absent(&self) -> bool {
        !self.connected && matches!(self.source.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused)
    }
}

fn prepare_request(hello: &Value, cmd: &Value, expected: Option<&str>) -> io::Result<(String, Value)> {
    let id = hello["instance"].as_str().filter(|s| !s.is_empty() && s.len() <= 128).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing bridge instance identity"))?;
    if hello["protocol"] != 1 || expected.is_some_and(|old| old != id) {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Pixelferrite window changed or protocol is unsupported; restart the MCP client to select a new document"));
    }
    Ok((id.to_owned(), json!({"instance": id, "command": cmd})))
}

fn parse_reply(reply: &Value, instance: &str) -> io::Result<Result<Value, String>> {
    if reply["instance"] == instance {
        match reply["ok"].as_bool() {
            Some(true) if reply.get("result").is_some() => return Ok(Ok(reply["result"].clone())),
            Some(false) if reply["error"].is_string() => return Ok(Err(reply["error"].as_str().unwrap().to_owned())),
            _ => {},
        }
    }
    Err(io::Error::new(io::ErrorKind::InvalidData, "invalid bridge response"))
}

/// A new socket carries an instance handshake before any command is sent. MCP
/// pins that identity: reconnecting after a restart cannot target a new window.
pub(crate) fn call(port: u16, cmd: &Value, expected: Option<&str>) -> Result<CallReply, CallError> {
    #[cfg(unix)]
    { call_with(cmd, expected, || unix::connect(port)) }
    #[cfg(not(unix))]
    {
        let _ = (port, cmd, expected);
        Err(CallError { source: io::Error::new(io::ErrorKind::Unsupported, "the private MCP bridge requires macOS or Linux; use --headless"), connected: false, instance: None, may_have_executed: false })
    }
}

#[cfg(unix)]
fn call_with(cmd: &Value, expected: Option<&str>, connect: impl FnOnce() -> io::Result<std::os::unix::net::UnixStream>) -> Result<CallReply, CallError> {
    let mut connected = false;
    let mut instance = None;
    let mut may_have_executed = false;
    let result = (|| -> io::Result<CallReply> {
        let stream = connect()?;
        connected = true;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut reader = BufReader::new(stream);
        let hello = read_json(&mut reader, 4096, IO_TIMEOUT)?;
        let (id, request) = prepare_request(&hello, cmd, expected)?;
        instance = Some(id.clone());
        // Mark before writing: even a partial write error has an uncertain outcome.
        may_have_executed = true;
        write_json(reader.get_mut(), &request, MAX_REQUEST_BYTES)?;
        reader.get_ref().set_read_timeout(Some(COMMAND_TIMEOUT + IO_TIMEOUT))?;
        let reply = read_json(&mut reader, MAX_REPLY_BYTES, COMMAND_TIMEOUT + IO_TIMEOUT)?;
        let result = parse_reply(&reply, &id)?;
        Ok(CallReply { instance: id, result })
    })();
    result.map_err(|source| CallError { source, connected, instance, may_have_executed })
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::{ffi::OsStrExt, fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt}, net::{UnixListener, UnixStream}};
    use std::path::{Path, PathBuf};

    fn denied(message: &str) -> io::Error { io::Error::new(io::ErrorKind::PermissionDenied, message) }

    pub(super) fn private_dir(path: &Path, create: bool) -> io::Result<PathBuf> {
        if create { fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?; }
        let metadata = fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no arguments or preconditions.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o077 != 0 {
            return Err(denied("MCP socket directory must be owned by you, not a symlink, and mode 0700"));
        }
        let canonical = fs::canonicalize(path)?;
        for ancestor in canonical.ancestors().skip(1) {
            let metadata = fs::metadata(ancestor)?;
            if (metadata.uid() != uid && metadata.uid() != 0) || (metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0) {
                return Err(denied("MCP socket directory has an unsafe writable or foreign-owned ancestor"));
            }
        }
        Ok(canonical)
    }

    fn socket_path(port: u16, create: bool) -> io::Result<PathBuf> {
        let config = crate::store::Store::standard().config_dir;
        if create { pf_core::io::atomic::create_private_dir(&config)?; }
        socket_path_for_dir(&config, port, create)
    }

    pub(super) fn socket_path_for_dir(config: &Path, port: u16, create: bool) -> io::Result<PathBuf> {
        let dir = private_dir(&config.join("bridge"), create)?;
        let path = dir.join(format!("{port}.sock"));
        // sockaddr_un is limited to 104 bytes on macOS. Keep the normal path
        // for compatibility; long custom settings folders use a short private
        // runtime directory keyed by their filesystem identity, without hash
        // collisions or bypassing the original directory's permission checks.
        if path.as_os_str().as_bytes().len() < 100 { return Ok(path); }
        let metadata = fs::metadata(&dir)?;
        let runtime = Path::new("/tmp").join(format!("pixelferrite-mcp-{}-{:x}-{:x}", metadata.uid(), metadata.dev(), metadata.ino()));
        Ok(private_dir(&runtime, create)?.join(format!("{port}.sock")))
    }

    fn socket_metadata(path: &Path) -> io::Result<fs::Metadata> {
        let metadata = fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no arguments or preconditions.
        if !metadata.file_type().is_socket() || metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
            return Err(denied("MCP socket is not a private socket owned by you"));
        }
        Ok(metadata)
    }

    struct SocketFile { path: PathBuf, device: u64, inode: u64 }
    impl Drop for SocketFile {
        fn drop(&mut self) {
            if fs::symlink_metadata(&self.path).is_ok_and(|m| m.dev() == self.device && m.ino() == self.inode) {
                let _ = fs::remove_file(&self.path);
            }
        }
    }

    pub(super) fn start(port: u16, ctx: egui::Context) -> io::Result<Bridge> {
        let path = socket_path(port, true)?;
        start_at(path, ctx)
    }

    pub(super) fn start_at(path: PathBuf, ctx: egui::Context) -> io::Result<Bridge> {
        private_dir(path.parent().ok_or_else(|| denied("MCP socket has no parent directory"))?, false)?;
        // Keep the lock while listening, so simultaneous launches cannot remove
        // each other's socket during stale-file cleanup. Never follow a symlink.
        let lock_file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(path.with_extension("lock"))?;
        let lock_metadata = lock_file.metadata()?;
        // SAFETY: geteuid has no arguments or preconditions.
        if !lock_metadata.is_file() || lock_metadata.uid() != unsafe { libc::geteuid() } || lock_metadata.mode() & 0o077 != 0 {
            return Err(denied("MCP lock file is not private and owned by you"));
        }
        // SAFETY: the file descriptor is live, and these are valid flock flags.
        if unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::new(io::ErrorKind::AddrInUse, "another window already owns this MCP channel"));
        }
        if path.exists() {
            let before = socket_metadata(&path)?;
            match connect_path(&path) {
                Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
                    let now = socket_metadata(&path)?;
                    if (before.dev(), before.ino()) != (now.dev(), now.ino()) { return Err(io::Error::other("MCP socket changed while starting")); }
                    fs::remove_file(&path)?;
                }
                _ => return Err(io::Error::new(io::ErrorKind::AddrInUse, "another window already owns this MCP channel")),
            }
        }
        let listener = UnixListener::bind(&path)?;
        let metadata = fs::symlink_metadata(&path)?;
        let socket_file = SocketFile { path: path.clone(), device: metadata.dev(), inode: metadata.ino() };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let mut random = [0u8; 16];
        fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
        let instance = random.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let (tx, rx) = sync_channel(MAX_QUEUED);
        let stop = Arc::new(AtomicBool::new(false));
        let bridge = Bridge { rx, stop: stop.clone(), ctx: ctx.clone() };
        let connections = Arc::new(AtomicUsize::new(0));
        std::thread::Builder::new().name("pixelferrite-mcp-listener".into()).spawn(move || {
            let _lock_file = lock_file;
            let _socket_file = socket_file;
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if connections.load(Ordering::Acquire) >= MAX_CONNECTIONS { continue; }
                        connections.fetch_add(1, Ordering::AcqRel);
                        let (tx, ctx, instance, connections) = (tx.clone(), ctx.clone(), instance.clone(), connections.clone());
                        struct Count(Arc<AtomicUsize>);
                        impl Drop for Count { fn drop(&mut self) { self.0.fetch_sub(1, Ordering::AcqRel); } }
                        let count = Count(connections);
                        let _ = std::thread::Builder::new().name("pixelferrite-mcp-client".into()).spawn(move || {
                            let _count = count;
                            let _ = handle(stream, &instance, tx, ctx);
                        });
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(25)),
                    Err(_) => break,
                }
            }
        })?;
        Ok(bridge)
    }

    fn handle(stream: UnixStream, instance: &str, tx: SyncSender<Pending>, ctx: egui::Context) -> io::Result<()> {
        // macOS accepted sockets may inherit the nonblocking listener flag.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        let mut reader = BufReader::new(stream);
        write_json(reader.get_mut(), &json!({"protocol": 1, "instance": instance}), 4096)?;
        let frame = read_frame_inner(&mut reader, MAX_REQUEST_BYTES, Some(Instant::now() + IO_TIMEOUT))?.ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "peer closed the connection"))?;
        let request: Value = serde_json::from_slice(&frame).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        if request["instance"] != instance || !request["command"].is_object() {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid bridge command envelope"));
        }
        let (reply, wait) = channel();
        let cancelled = Arc::new(AtomicBool::new(false));
        let pending = Pending { cmd: request["command"].clone(), reply, expires: Instant::now() + COMMAND_TIMEOUT, cancelled: cancelled.clone() };
        let result = match tx.try_send(pending) {
            Ok(()) => {
                ctx.request_repaint();
                match wait.recv_timeout(COMMAND_TIMEOUT) {
                    Ok(result) => result,
                    Err(_) => {
                        cancelled.store(true, Ordering::Release);
                        Err("Pixelferrite did not answer; the command outcome is unknown. Look at the document before retrying.".to_owned())
                    }
                }
            }
            Err(_) => Err("Pixelferrite is busy or closing; command was not queued".to_owned()),
        };
        let msg = match result {
            Ok(v) => json!({"instance": instance, "ok": true, "result": v}),
            Err(e) => json!({"instance": instance, "ok": false, "error": e}),
        };
        write_json(reader.get_mut(), &msg, MAX_REPLY_BYTES)
    }

    pub(super) fn connect(port: u16) -> io::Result<UnixStream> {
        let path = socket_path(port, false)?;
        connect_at(&path)
    }

    pub(super) fn connect_at(path: &Path) -> io::Result<UnixStream> {
        private_dir(path.parent().ok_or_else(|| denied("MCP socket has no parent directory"))?, false)?;
        socket_metadata(path)?;
        connect_path(path)
    }

    /// Nonblocking connect also bounds a full socket backlog, unlike the
    /// standard library's blocking UnixStream::connect.
    fn connect_path(path: &Path) -> io::Result<UnixStream> {
        // SAFETY: zero is valid for sockaddr_un, whose initialized fields and
        // exact buffer length are passed to connect below.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        let bytes = path.as_os_str().as_bytes();
        if bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "MCP socket path is too long or invalid"));
        }
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        for (out, &byte) in address.sun_path.iter_mut().zip(bytes) { *out = byte as libc::c_char; }
        #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd", target_os = "openbsd", target_os = "netbsd", target_os = "dragonfly"))]
        { address.sun_len = std::mem::size_of_val(&address) as u8; }
        // SAFETY: socket returns an owned fd, immediately wrapped for RAII.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 { return Err(io::Error::last_os_error()); }
        // SAFETY: fd is newly created and owned by this function.
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        // SAFETY: the fd is live and F_SETFD takes an integer flag value.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 { return Err(io::Error::last_os_error()); }
        stream.set_nonblocking(true)?;
        // SAFETY: address points to an initialized sockaddr_un of the given size.
        if unsafe { libc::connect(fd, (&address as *const libc::sockaddr_un).cast(), std::mem::size_of_val(&address) as libc::socklen_t) } < 0 {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(libc::EINPROGRESS) { return Err(e); }
            let mut pollfd = libc::pollfd { fd: stream.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
            // SAFETY: poll receives one valid descriptor and a finite timeout.
            let ready = unsafe { libc::poll(&mut pollfd, 1, 400) };
            if ready < 0 { return Err(io::Error::last_os_error()); }
            if ready == 0 { return Err(io::Error::new(io::ErrorKind::TimedOut, "MCP connect timed out")); }
            if let Some(e) = stream.take_error()? { return Err(e); }
        }
        stream.set_nonblocking(false)?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_bounded_complete_and_independent() {
        let mut reader = BufReader::with_capacity(2, &b"{}\n[]\n"[..]);
        assert_eq!(read_frame(&mut reader, 3).unwrap().unwrap(), b"{}\n");
        assert_eq!(read_frame(&mut reader, 3).unwrap().unwrap(), b"[]\n");
        assert!(read_frame(&mut reader, 3).unwrap().is_none());
        let mut reader = BufReader::with_capacity(2, &b"123456789\n"[..]);
        assert_eq!(read_frame(&mut reader, 3).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(read_frame(&mut &b"{}"[..], 3).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn oversized_writes_send_nothing() {
        let mut output = Vec::new();
        assert!(write_json(&mut output, &json!({"long": "value"}), 4).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn protocol_rejects_missing_identity_changed_windows_and_bad_replies() {
        let cmd = json!({"op": "save"});
        assert!(prepare_request(&json!({"protocol": 1}), &cmd, None).is_err());
        assert!(prepare_request(&json!({"protocol": 2, "instance": "window-a"}), &cmd, None).is_err());
        assert!(prepare_request(&json!({"protocol": 1, "instance": "window-b"}), &cmd, Some("window-a")).is_err());
        let (id, request) = prepare_request(&json!({"protocol": 1, "instance": "window-a"}), &cmd, Some("window-a")).unwrap();
        assert_eq!(request["instance"], id);
        assert_eq!(request["command"], cmd);
        for bad in [json!({"instance": "window-b", "ok": true, "result": {}}), json!({"instance": "window-a", "ok": true}), json!({"instance": "window-a", "ok": false, "error": 12})] {
            assert!(parse_reply(&bad, "window-a").is_err());
        }
        assert_eq!(parse_reply(&json!({"instance": "window-a", "ok": false, "error": "model rejected"}), "window-a").unwrap().unwrap_err(), "model rejected");
    }

    #[test]
    fn expired_frame_deadline_rejects_even_available_input() {
        let mut reader = &b"{}\n"[..];
        let error = read_frame_inner(&mut reader, 10, Some(Instant::now() - Duration::from_secs(1))).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(reader, b"{}\n");
    }

    #[test]
    fn expired_and_cancelled_commands_do_not_execute() {
        let (tx, rx) = sync_channel(4);
        let (reply, wait) = channel();
        tx.send(Pending { cmd: json!({}), reply, expires: Instant::now() - Duration::from_secs(1), cancelled: Arc::new(AtomicBool::new(false)) }).unwrap();
        let bridge = Bridge { rx, stop: Arc::new(AtomicBool::new(false)), ctx: egui::Context::default() };
        bridge.serve(|_| panic!("expired request executed"));
        assert!(wait.recv().unwrap().is_err());
        let (reply, wait) = channel();
        tx.send(Pending { cmd: json!({}), reply, expires: Instant::now() + Duration::from_secs(1), cancelled: Arc::new(AtomicBool::new(true)) }).unwrap();
        bridge.serve(|_| panic!("cancelled request executed"));
        assert!(wait.recv().unwrap().is_err());
    }

    #[test]
    #[cfg(unix)]
    fn socket_directory_rejects_symlinks_and_exposed_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = std::env::temp_dir().join(format!("pixelferrite-bridge-test-{}", std::process::id()));
        let path = root.join("private");
        let private = unix::private_dir(&path, true).unwrap();
        assert_eq!(std::fs::metadata(&private).unwrap().permissions().mode() & 0o777, 0o700);
        let alias = root.join("alias");
        symlink(&path, &alias).unwrap();
        assert_eq!(unix::private_dir(&alias, false).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(unix::private_dir(&path, false).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn long_config_paths_use_distinct_private_short_sockets() {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::path::Path::new("/tmp").join(format!("pf-long-{}-{nonce:x}", std::process::id()));
        let first = root.join("a".repeat(120));
        let second = root.join("b".repeat(120));
        let a = unix::socket_path_for_dir(&first, 47821, true).unwrap();
        let b = unix::socket_path_for_dir(&second, 47821, true).unwrap();
        assert_ne!(a, b);
        assert!(a.as_os_str().as_bytes().len() < 100);
        assert_eq!(a, unix::socket_path_for_dir(&first, 47821, false).unwrap());
        assert_eq!(std::fs::metadata(a.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
        let listener = std::os::unix::net::UnixListener::bind(&a).unwrap();
        assert!(std::os::unix::net::UnixStream::connect(&a).is_ok());
        drop(listener);
        std::fs::remove_dir_all(a.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(b.parent().unwrap()).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn private_socket_round_trip_and_restart_identity() {
        use std::os::unix::fs::PermissionsExt;
        // Keep the path below macOS's sockaddr_un limit, independent of the
        // long per-user TMPDIR. Never use the configured application socket.
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::path::PathBuf::from("/tmp").join(format!("pf-mcp-{}-{nonce:x}", std::process::id()));
        let dir = unix::private_dir(&root, true).unwrap();
        let path = dir.join("test.sock");
        let bridge = unix::start_at(path.clone(), egui::Context::default()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(unix::start_at(path.clone(), egui::Context::default()).is_err());

        let wrong = call_with(&json!({"op": "must_not_run"}), Some("different-window"), || unix::connect_at(&path)).unwrap_err();
        assert!(wrong.connected);
        assert!(!wrong.may_have_executed);
        assert!(bridge.rx.try_recv().is_err());

        let client_path = path.clone();
        let worker = std::thread::spawn(move || call_with(&json!({"op": "test_echo", "value": 42}), None, || unix::connect_at(&client_path)));
        let deadline = Instant::now() + Duration::from_secs(3);
        while !worker.is_finished() && Instant::now() < deadline {
            bridge.serve(|cmd| { assert_eq!(cmd["op"], "test_echo"); Ok(json!({"echo": cmd["value"]})) });
            std::thread::sleep(Duration::from_millis(5));
        }
        let completed = worker.is_finished();
        drop(bridge);
        let reply = worker.join().unwrap().unwrap();
        assert!(completed, "isolated bridge did not answer within three seconds");
        assert_eq!(reply.result.unwrap()["echo"], 42);

        let deadline = Instant::now() + Duration::from_secs(2);
        while path.exists() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(5)); }
        assert!(!path.exists(), "closing a bridge must remove its socket");
        let restarted = unix::start_at(path.clone(), egui::Context::default()).unwrap();
        let error = call_with(&json!({"op": "must_not_run"}), Some(&reply.instance), || unix::connect_at(&path)).unwrap_err();
        assert!(!error.may_have_executed);
        assert!(restarted.rx.try_recv().is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(unix::connect_at(&path).unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        drop(restarted);
        let deadline = Instant::now() + Duration::from_secs(2);
        while path.exists() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(5)); }
        std::fs::remove_dir_all(root).unwrap();
    }
}
