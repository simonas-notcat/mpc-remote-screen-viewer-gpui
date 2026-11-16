//! Same-user local IPC. One worker, one bounded request per connection, no HTTP.
use super::{Reply, Request, Teacher};
use std::{
    fs::{self, DirBuilder},
    io::{self, Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const MAX_REQUEST: usize = 16 * 1024;
const MAX_RESPONSE: usize = 32 * 1024 * 1024;
const POLL: Duration = Duration::from_millis(10);
fn uid() -> u32 {
    // SAFETY: geteuid has no arguments, pointers, or side effects.
    unsafe { libc::geteuid() }
}
pub fn default_socket_path() -> PathBuf {
    std::env::var_os("MPC_TEACHER_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir()
                .join(format!("mpc-teacher-{}", uid()))
                .join("viewer.sock")
        })
}
fn private_parent(path: &Path, create: bool) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::other("Teacher socket path must be absolute"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Socket has no parent"))?;
    if create {
        match DirBuilder::new().mode(0o700).create(parent) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e),
        }
    }
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "Teacher socket parent must be a same-user directory with mode 0700 (no symlink)",
        ));
    }
    Ok(())
}
fn socket_metadata(path: &Path) -> io::Result<fs::Metadata> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_socket() || meta.uid() != uid() || meta.mode() & 0o077 != 0 {
        return Err(io::Error::other(
            "Teacher endpoint must be a private same-user Unix socket",
        ));
    }
    Ok(meta)
}
struct SocketGuard {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub struct Bridge {
    teacher: Arc<Teacher>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    path: PathBuf,
}
impl Bridge {
    pub fn start(path: PathBuf) -> io::Result<Self> {
        private_parent(&path, true)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {
                socket_metadata(&path)?;
                match UnixStream::connect(&path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "Another AI teacher viewer is already listening",
                        ));
                    }
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        fs::remove_file(&path)?
                    }
                    Err(e) => return Err(e),
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        let listener = UnixListener::bind(&path)?;
        let meta = fs::symlink_metadata(&path)?;
        let guard = SocketGuard {
            path: path.clone(),
            dev: meta.dev(),
            ino: meta.ino(),
        };
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        listener.set_nonblocking(true)?;
        let teacher = Arc::new(Teacher::new()?);
        let controller = teacher.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let worker = thread::Builder::new()
            .name("ai-teacher-ipc".into())
            .spawn(move || {
                let _guard = guard;
                while !cancelled.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            let result = (|| {
                                stream.set_nonblocking(true)?;
                                let bytes = read_frame(
                                    &mut stream,
                                    MAX_REQUEST,
                                    Duration::from_millis(500),
                                )?;
                                let request = serde_json::from_slice::<Request>(&bytes)
                                    .map_err(io::Error::other)?;
                                if cancelled.load(Ordering::Acquire) {
                                    return Err(io::Error::other("Teacher disabled"));
                                }
                                controller.handle(request).map_err(io::Error::other)
                            })();
                            let reply = result.unwrap_or_else(|e| Reply::Error {
                                message: e.to_string(),
                            });
                            if !cancelled.load(Ordering::Acquire)
                                && let Ok(bytes) = serde_json::to_vec(&reply)
                            {
                                let _ = write_frame(
                                    &mut stream,
                                    &bytes,
                                    MAX_RESPONSE,
                                    Duration::from_secs(2),
                                );
                            }
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                        Err(_) => break,
                    }
                }
            })?;
        Ok(Self {
            teacher,
            stop,
            worker: Some(worker),
            path,
        })
    }
    pub fn teacher(&self) -> &Arc<Teacher> {
        &self.teacher
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Immediate local revocation. Joining/cleanup can run off the GPUI thread.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
        if let Some(control) = self.teacher.control() {
            control.revoke("AI teacher sharing disabled locally");
        }
        self.teacher.set_source(None);
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn request(path: &Path, request: &Request) -> Result<Reply, String> {
    (|| -> io::Result<Reply> {
        private_parent(path, false)?;
        socket_metadata(path)?;
        let mut stream = UnixStream::connect(path)?;
        stream.set_nonblocking(true)?;
        let bytes = serde_json::to_vec(request).map_err(io::Error::other)?;
        write_frame(&mut stream, &bytes, MAX_REQUEST, Duration::from_secs(2))?;
        let bytes = read_frame(&mut stream, MAX_RESPONSE, Duration::from_secs(10))?;
        serde_json::from_slice(&bytes).map_err(io::Error::other)
    })().map_err(|e| format!("AI teacher viewer unavailable: {e}. Enable MCP with the robot button (Toggle MCP) in the viewer's top toolbar."))
}

/// Fixed big-endian u32 length, validated before allocation. A wall-clock deadline
/// prevents a slow peer extending an ordinary per-read timeout indefinitely.
fn read_frame(stream: &mut UnixStream, limit: usize, timeout: Duration) -> io::Result<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    let mut header = [0u8; 4];
    read_until(stream, &mut header, deadline)?;
    let len = u32::from_be_bytes(header) as usize;
    if len == 0 || len > limit {
        return Err(io::Error::other("IPC frame exceeds its size limit"));
    }
    let mut bytes = vec![0u8; len];
    read_until(stream, &mut bytes, deadline)?;
    Ok(bytes)
}
fn read_until(stream: &mut UnixStream, mut bytes: &mut [u8], deadline: Instant) -> io::Result<()> {
    while !bytes.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "IPC read deadline"));
        }
        match stream.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Incomplete IPC frame",
                ));
            }
            Ok(n) => bytes = &mut bytes[n..],
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
            Err(e) => return Err(e),
        }
    }
    Ok(())
}
fn write_frame(
    stream: &mut UnixStream,
    bytes: &[u8],
    limit: usize,
    timeout: Duration,
) -> io::Result<()> {
    if bytes.is_empty() || bytes.len() > limit {
        return Err(io::Error::other("IPC frame exceeds its size limit"));
    }
    let deadline = Instant::now() + timeout;
    for mut bytes in [&(bytes.len() as u32).to_be_bytes()[..], bytes] {
        while !bytes.is_empty() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "IPC write deadline",
                ));
            }
            match stream.write(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "Closed IPC connection",
                    ));
                }
                Ok(n) => bytes = &bytes[n..],
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(POLL),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                Err(e) => return Err(e),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "mpc-ipc-test-{}-{}",
            std::process::id(),
            crate::teacher::tests::unique()
        ));
        DirBuilder::new().mode(0o700).create(&path).unwrap();
        path
    }
    #[test]
    fn private_bridge_lifecycle_and_live_socket_not_replaced() {
        let dir = directory();
        let path = dir.join("viewer.sock");
        let bridge = Bridge::start(path.clone()).unwrap();
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        assert!(matches!(
            request(&path, &Request::GetState).unwrap(),
            Reply::Ok { .. }
        ));
        assert!(Bridge::start(path.clone()).is_err());
        assert!(request(&path, &Request::ClearHighlight).is_ok());
        drop(bridge);
        assert!(!path.exists());
        fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn unsafe_parent_and_non_socket_are_never_replaced() {
        let dir = directory();
        let path = dir.join("viewer.sock");
        fs::write(&path, b"keep").unwrap();
        assert!(Bridge::start(path.clone()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(&path).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Bridge::start(path).is_err());
        fs::remove_dir(dir).unwrap();
    }
    #[test]
    fn frame_limits_partial_reads_and_deadlines() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        b.set_nonblocking(true).unwrap();
        a.write_all(&100u32.to_be_bytes()).unwrap();
        assert!(read_frame(&mut b, 10, Duration::from_secs(1)).is_err());
        assert_eq!(
            read_frame(&mut b, 10, Duration::from_millis(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        a.write_all(&[0, 0, 0, 2, b'{']).unwrap();
        drop(a);
        assert_eq!(
            read_frame(&mut b, 10, Duration::from_secs(1))
                .unwrap_err()
                .kind(),
            io::ErrorKind::UnexpectedEof
        );
    }
    #[test]
    fn stale_socket_recovered_and_unknown_mutations_rejected() {
        let dir = directory();
        let path = dir.join("viewer.sock");
        drop(UnixListener::bind(&path).unwrap());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let bridge = Bridge::start(path.clone()).unwrap();
        let mut stream = UnixStream::connect(&path).unwrap();
        stream.set_nonblocking(true).unwrap();
        write_frame(
            &mut stream,
            br#"{"op":"input","events":[]}"#,
            MAX_REQUEST,
            Duration::from_secs(1),
        )
        .unwrap();
        let reply = read_frame(&mut stream, MAX_RESPONSE, Duration::from_secs(2)).unwrap();
        assert!(matches!(
            serde_json::from_slice::<Reply>(&reply).unwrap(),
            Reply::Error { .. }
        ));
        drop(bridge);
        fs::remove_dir(dir).unwrap();
    }
}
