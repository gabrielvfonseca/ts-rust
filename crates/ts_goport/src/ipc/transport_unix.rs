//! Port of internal/ipc/transport_unix.go (`//go:build !windows`;
//! internal/api/transport_unix.go before tsgo#4712).
//!
//! PORT: Go `net.Listen("unix", path)` and `net.UnixConn` are
//! `std::os::unix::net`. Errors are Go's `*net.OpError` texts ("op unix
//! addr: syscall: errno text").

use crate::ipc::prelude::*;

use crate::fswatch::syscall::io_error_text;
use crate::gostd::{GoError, errors};
use crate::ipc::transport::{ERR_NET_CLOSED, NetListener, PipeListener, ReadWriteCloser};
use std::io::ErrorKind;
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::{Arc, Mutex};

// Go: ipc/transport_unix.go:14 newPipeListener (ts#64818)
// PORT: Go's doc comment is gone at N'. `dir` must not exist: it becomes a
// private directory with the socket "socket" in it.
pub fn new_pipe_listener(dir: &str) -> Result<PipeListener, GoError> {
    if let Err(err) = os_mkdir(dir, 0o700) {
        return Err(wrap("create private socket directory", err));
    }
    listen_in_private_pipe_directory(dir.to_string())
}

// Go: ipc/transport_unix.go:21 newPrivatePipeListener (ts#64818)
// PORT: Go ignores the name on Unix; the directory is a new temp directory.
pub fn new_private_pipe_listener(_name: &str) -> Result<PipeListener, GoError> {
    let dir = match mkdir_temp("tsgo-api-") {
        Ok(dir) => dir,
        Err(err) => return Err(wrap("create private socket directory", err)),
    };
    listen_in_private_pipe_directory(dir)
}

// Go: ipc/transport_unix.go:29 listenInPrivatePipeDirectory (ts#64818)
fn listen_in_private_pipe_directory(dir: String) -> Result<PipeListener, GoError> {
    let socket_path = path_join(&dir, "socket");
    let cleanup = {
        let (socket_path, dir) = (socket_path.clone(), dir.clone());
        move || -> Result<(), GoError> {
            // Go: errors.Is(err, fs.ErrNotExist) is a nil error.
            let remove = |path: &str| match os_remove(path) {
                Err(err) if err.kind() != ErrorKind::NotFound => {
                    Some(path_error("remove", path, &err))
                }
                _ => None,
            };
            let socket_err = remove(&socket_path);
            let dir_err = remove(&dir);
            errors::join([socket_err, dir_err]).map_or(Ok(()), Err)
        }
    };

    if let Err(err) = os_chmod(&dir, 0o700) {
        return Err(join_errors([
            Some(wrap("set private socket directory permissions", err)),
            cleanup().err(),
        ]));
    }

    let listener = match listen(&socket_path) {
        Ok(listener) => listener,
        Err(err) => {
            return Err(join_errors([
                Some(wrap("listen on private socket", err)),
                cleanup().err(),
            ]));
        }
    };
    if let Err(err) = os_chmod(&socket_path, 0o600) {
        return Err(join_errors([
            Some(wrap("set private socket permissions", err)),
            listener.close().err(),
            cleanup().err(),
        ]));
    }
    Ok((Box::new(listener), Some(Box::new(cleanup))))
}

/// Go `fmt.Errorf(text + ": %w", err)`.
fn wrap(text: &str, err: GoError) -> GoError {
    errors::errorf(format!("{text}: {}", err.error()), vec![err])
}

/// Go `errors.Join(errs...)` where the first error is not nil.
fn join_errors<const N: usize>(errs: [Option<GoError>; N]) -> GoError {
    errors::join(errs).expect("the first error is not nil")
}

/// Go `&PathError{Op: op, Path: path, Err: errno}.Error()`.
fn path_error(op: &str, path: &str, err: &std::io::Error) -> GoError {
    errors::new(format!("{op} {path}: {}", errno_text(err)))
}

/// The text of Go's errno for a failed call.
/// PORT: std fails with no errno and `InvalidInput` where Go's syscall
/// gives EINVAL: a path with a NUL byte (Go `syscall.BytePtrFromString`)
/// or a socket name that is too long.
fn errno_text(err: &std::io::Error) -> String {
    match err.raw_os_error() {
        None if err.kind() == ErrorKind::InvalidInput => "invalid argument".to_string(),
        _ => io_error_text(err),
    }
}

// Go `os.Mkdir(name, perm)` (go1.27.1 os/file.go): the mode is masked by
// the umask.
fn os_mkdir(name: &str, perm: u32) -> Result<(), GoError> {
    std::fs::DirBuilder::new()
        .mode(perm)
        .create(name)
        .map_err(|err| path_error("mkdir", name, &err))
}

// Go `os.Chmod(name, mode)` (go1.27.1 os/file_posix.go).
fn os_chmod(name: &str, mode: u32) -> Result<(), GoError> {
    std::fs::set_permissions(name, std::fs::Permissions::from_mode(mode))
        .map_err(|err| path_error("chmod", name, &err))
}

// Go `os.Remove(name)` (go1.27.1 os/file_unix.go): unlink, then rmdir.
// When both fail, rmdir of a file gives ENOTDIR, and the unlink error is
// the real one.
fn os_remove(name: &str) -> std::io::Result<()> {
    let Err(err) = std::fs::remove_file(name) else {
        return Ok(());
    };
    let Err(dir_err) = std::fs::remove_dir(name) else {
        return Ok(());
    };
    if dir_err.kind() != ErrorKind::NotADirectory {
        return Err(dir_err);
    }
    Err(err)
}

// Go `os.MkdirTemp("", pattern)` (go1.27.1 os/tempfile.go) for a pattern
// with no '*' and no separator: the directory `<os.TempDir()>/<pattern><n>`,
// mode 0o700, with a new random `uint32` n until the name is free.
fn mkdir_temp(pattern: &str) -> Result<String, GoError> {
    let dir = os_temp_dir();
    // Go joinPath
    let prefix = if dir.ends_with('/') {
        format!("{dir}{pattern}")
    } else {
        format!("{dir}/{pattern}")
    };
    let mut tries = 0;
    loop {
        let name = format!("{prefix}{}", next_random());
        let err = match std::fs::DirBuilder::new().mode(0o700).create(&name) {
            Ok(()) => return Ok(name),
            Err(err) => err,
        };
        if err.kind() == ErrorKind::AlreadyExists {
            tries += 1;
            if tries < 10000 {
                continue;
            }
            // Go `fs.ErrExist`
            return Err(errors::new(format!(
                "mkdirtemp {prefix}*: file already exists"
            )));
        }
        if err.kind() == ErrorKind::NotFound
            && let Err(stat_err) = std::fs::metadata(&dir)
            && stat_err.kind() == ErrorKind::NotFound
        {
            return Err(path_error("stat", &dir, &stat_err));
        }
        return Err(path_error("mkdir", &name, &err));
    }
}

// Go `os.nextRandom`: a random `uint32` in decimal.
// PORT: Go `runtime_rand()`; the port has no rand crate and takes the bits
// from std's randomly keyed hasher (as `generate_api_pipe_name`).
fn next_random() -> String {
    use std::hash::{BuildHasher, Hasher};
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (bits as u32).to_string()
}

/// Go `net.Listen("unix", path)`.
fn listen(path: &str) -> Result<UnixPipeListener, GoError> {
    // PORT: the wake pair of `UnixPipeListener` (no Go step) comes first,
    // so its failure leaves no socket file. Its text is Go's for a failed
    // `socket` call.
    let (wake, wake_writer) = UnixStream::pair().map_err(|err| {
        errors::new(format!(
            "listen unix {path}: socket: {}",
            io_error_text(&err)
        ))
    })?;
    let listener = listen_unix(path).and_then(|listener| {
        listener.set_nonblocking(true)?;
        Ok(listener)
    });
    match listener {
        Ok(listener) => Ok(UnixPipeListener {
            state: Mutex::new(Some(UnixListenerState {
                listener: Arc::new(listener),
                wake: wake_writer,
            })),
            wake,
            path: path.to_string(),
        }),
        // Go: net/sock_posix.go listenStream, `os.NewSyscallError("bind",
        // err)`, in `&OpError{Op: "listen", Net: "unix", Addr: laddr}`.
        // PORT: std checks the name length before the socket call and
        // fails with no errno, where Go's `syscall.Bind` gives EINVAL. A
        // `socket` or `listen` failure (and the port's `set_nonblocking`)
        // also prints "bind" here.
        Err(err) => Err(errors::new(format!(
            "listen unix {path}: bind: {}",
            errno_text(&err)
        ))),
    }
}

/// Go `net.Listen("unix", path)`. On Linux a name that starts with '@' is
/// an abstract name, with no file (go1.27.1 syscall/syscall_linux.go
/// `SockaddrUnix.sockaddr`: the '@' becomes a NUL, and the address has no
/// trailing NUL). `from_abstract_name` makes the same address, and fails
/// with no errno where Go's name is longer than 108 bytes (EINVAL).
fn listen_unix(path: &str) -> std::io::Result<UnixListener> {
    #[cfg(target_os = "linux")]
    if let Some(name) = path.strip_prefix('@') {
        use std::os::linux::net::SocketAddrExt;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(name)?;
        return UnixListener::bind_addr(&addr);
    }
    UnixListener::bind(path)
}

// Go: ipc/transport_unix.go:58 GeneratePipePath
// GeneratePipePath returns a platform-appropriate pipe path for the given name.
// PORT: no caller on Unix since ts#64818 (`newPrivatePipeListener` makes a
// temp directory), as in Go.
pub fn generate_pipe_path(name: &str) -> String {
    path_join(&os_temp_dir(), name)
}

// Go `os.TempDir` on Unix: $TMPDIR, or "/tmp" when it is empty.
fn os_temp_dir() -> String {
    match std::env::var("TMPDIR") {
        Ok(dir) if !dir.is_empty() => dir,
        _ => "/tmp".to_string(),
    }
}

// Go `filepath.Join(dir, name)` on Unix (`path.Join` before ts#64818):
// the non-empty elements joined by "/", then cleaned.
fn path_join(dir: &str, name: &str) -> String {
    let mut buf = String::new();
    for e in [dir, name] {
        if !buf.is_empty() || !e.is_empty() {
            if !buf.is_empty() {
                buf.push('/');
            }
            buf.push_str(e);
        }
    }
    if buf.is_empty() {
        return buf;
    }
    crate::frontend::vfs::filepath_clean(&buf)
}

// Go: internal/poll/sock_cloexec.go (accept4) and sys_cloexec.go (accept),
// the `errcall` of an accept error.
#[cfg(not(target_vendor = "apple"))]
const ACCEPT_CALL: &str = "accept4";
#[cfg(target_vendor = "apple")]
const ACCEPT_CALL: &str = "accept";

/// Go `*net.UnixListener` from `net.Listen("unix", path)`.
/// PORT: Go's `Accept` waits in the netpoller, and `Close` (internal/poll
/// fd_unix.go FD.Close) wakes it (`evict`); the descriptor closes when the
/// woken `Accept` drops its reference (`decref`). Here the listener is
/// non-blocking and `accept` waits in `poll` on the listener and on `wake`,
/// which `close` makes readable. `state` is `None` after `close`. An
/// `accept` holds its own reference to the listener, so the descriptor
/// closes when it returns. Go's `Close` waits for that; the port's does not
/// (the LSP server's `stop` joins its accept thread).
struct UnixPipeListener {
    state: Mutex<Option<UnixListenerState>>,
    /// The read end of the wake pair.
    wake: UnixStream,
    path: String,
}

struct UnixListenerState {
    listener: Arc<UnixListener>,
    /// The write end of the wake pair.
    wake: UnixStream,
}

impl UnixPipeListener {
    /// Go `pd.waitRead`: waits until the listener has a connection or
    /// `close` ran. A signal (EINTR) waits again, as in Go.
    fn wait(&self, listener: &UnixListener) -> Result<(), GoError> {
        use rustix::event::{PollFd, PollFlags, poll};
        let mut fds = [
            PollFd::new(listener, PollFlags::IN),
            PollFd::new(&self.wake, PollFlags::IN),
        ];
        loop {
            match poll(&mut fds, None) {
                Ok(_) => return Ok(()),
                Err(rustix::io::Errno::INTR) => continue,
                // PORT: no Go error (the netpoller does not fail here).
                Err(errno) => {
                    return Err(errors::new(format!(
                        "accept unix {}: poll: {}",
                        self.path,
                        io_error_text(&errno.into())
                    )));
                }
            }
        }
    }
}

impl NetListener for UnixPipeListener {
    // Go net.UnixListener.Accept (internal/poll/fd_unix.go FD.Accept)
    fn accept(&self) -> Result<Arc<dyn ReadWriteCloser>, GoError> {
        loop {
            // Go: `fd.readLock()` fails after Close.
            let listener = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .as_ref()
                .map(|state| state.listener.clone());
            let Some(listener) = listener else {
                return Err(closed_error("accept", &self.path));
            };
            // Go: net/fd_unix.go accept, `wrapSyscallError(errcall, err)`: std
            // and Go call accept4 on Linux and the BSDs, accept on darwin.
            let err = match listener.accept() {
                // PORT: macOS and the BSDs give the new socket the
                // listener's O_NONBLOCK; Go's connection waits in the
                // netpoller, the port's blocks.
                Ok((stream, _)) => match stream.set_nonblocking(false) {
                    Ok(()) => return Ok(Arc::new(UnixConn { stream })),
                    Err(err) => err,
                },
                Err(err) if err.kind() == ErrorKind::WouldBlock => {
                    self.wait(&listener)?;
                    continue;
                }
                // Go: a connection that closed in the queue; try again.
                Err(err) if err.kind() == ErrorKind::ConnectionAborted => continue,
                Err(err) => err,
            };
            return Err(errors::new(format!(
                "accept unix {}: {ACCEPT_CALL}: {}",
                self.path,
                io_error_text(&err)
            )));
        }
    }

    // Go net.UnixListener.Close: unlinks the socket file (listeners made by
    // `net.Listen` do; abstract names starting with '@' have no file), then
    // closes the socket, which wakes a waiting `Accept`.
    fn close(&self) -> Result<(), GoError> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        let Some(state) = state else {
            return Err(closed_error("close", &self.path));
        };
        if !self.path.starts_with('@') {
            let _ = std::fs::remove_file(&self.path);
        }
        // Go `evict`. The byte wakes the `poll` also while a child that
        // another thread spawns still has a copy of the write end.
        let _ = std::io::Write::write(&mut &state.wake, &[0]);
        // Go `decref`: the last reference closes the socket.
        drop(state);
        Ok(())
    }

    // Go `l.Addr().String()`: the socket name.
    fn addr(&self) -> String {
        self.path.clone()
    }
}

/// Go `&net.OpError{Op: op, Net: "unix", Addr: laddr, Err: net.ErrClosed}`.
fn closed_error(op: &str, path: &str) -> GoError {
    errors::errorf(
        format!("{op} unix {path}: {}", ERR_NET_CLOSED.error()),
        vec![ERR_NET_CLOSED.clone()],
    )
}

/// Go `*net.UnixConn`.
struct UnixConn {
    stream: UnixStream,
}

impl ReadWriteCloser for UnixConn {
    fn read(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut &self.stream, buf)
    }

    fn write(&self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::Write::write(&mut &self.stream, buf)
    }

    fn flush(&self) -> std::io::Result<()> {
        std::io::Write::flush(&mut &self.stream)
    }

    // PORT: Go closes the file descriptor. The stream is shared, so the port
    // shuts both directions down; the descriptor closes with the last owner.
    // macOS reports ENOTCONN when the peer has closed, where Go's close
    // succeeds, so that is not an error. Go's error is "close unix
    // <laddr>-><raddr>: <err>". The port's text has no addresses; callers
    // ignore it.
    fn close(&self) -> Result<(), GoError> {
        match self.stream.shutdown(Shutdown::Both) {
            Err(err) if err.kind() != std::io::ErrorKind::NotConnected => {
                Err(errors::new(format!("close unix: {}", io_error_text(&err))))
            }
            _ => Ok(()),
        }
    }
}

// Go: ipc/transport_unix_test.go (ts#64818)
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::transport::{new_pipe_transport, new_private_pipe_transport};

    // Go: ipc/transport_unix_test.go:17 privatePipeHelperEnv
    const PRIVATE_PIPE_HELPER_ENV: &str = "TSGO_PRIVATE_PIPE_HELPER";
    const PERMISSIONS_TEST: &str = "ipc::transport_unix::tests::test_pipe_transport_permissions";

    /// Go `filepath.Dir(path)` of a socket path.
    fn dir_of(path: &str) -> String {
        std::path::Path::new(path)
            .parent()
            .expect("a parent directory")
            .to_string_lossy()
            .into_owned()
    }

    // Go: ipc/transport_unix_test.go:19 TestPipeTransportClosePreservesReplacement
    #[test]
    fn test_pipe_transport_close_preserves_replacement() {
        let transport = new_private_pipe_transport("tsgo-api-test")
            .unwrap_or_else(|err| panic!("{}", err.error()));
        let socket_path = transport.path();
        if let Err(close_err) = transport.close() {
            panic!("{}", close_err.error());
        }

        let replacement = new_pipe_transport(&dir_of(&socket_path))
            .unwrap_or_else(|err| panic!("{}", err.error()));

        match transport.close() {
            Err(close_err) if errors::is(&close_err, &ERR_NET_CLOSED) => {}
            other => panic!(
                "second Close returned {:?}, want net.ErrClosed",
                other.map_err(|err| err.error())
            ),
        }
        // Go `net.DialTimeout("unix", socketPath, time.Second)`: a Unix
        // socket connects at once or fails.
        let conn = UnixStream::connect(&socket_path)
            .unwrap_or_else(|err| panic!("replacement socket is unreachable: {err}"));
        drop(conn);
        let _ = replacement.close();
    }

    // PORT: no Go test. Go's Close wakes an Accept that waits on another
    // goroutine (internal/poll/fd_unix.go FD.Close, `evict`): the Accept
    // fails with net.ErrClosed, and the cleanup removes the socket and its
    // directory (bump E L5 round 2, B2).
    #[test]
    fn close_ends_a_waiting_accept() {
        use std::sync::mpsc;
        use std::time::Duration;

        let transport = Arc::new(
            new_private_pipe_transport("tsgo-api-test")
                .unwrap_or_else(|err| panic!("{}", err.error())),
        );
        let socket_path = transport.path();
        let (accepted_tx, accepted) = mpsc::channel();
        let accept_transport = transport.clone();
        std::thread::spawn(move || {
            let _ = accepted_tx.send(accept_transport.accept().map(|_| ()));
        });
        // Gives the accept time to wait in poll. When it has not started
        // yet, it fails the same way.
        std::thread::sleep(Duration::from_millis(100));
        if let Err(err) = transport.close() {
            panic!("{}", err.error());
        }

        let err = accepted
            .recv_timeout(Duration::from_secs(10))
            .expect("close ended the accept")
            .expect_err("accept after close");
        assert!(errors::is(&err, &ERR_NET_CLOSED), "{}", err.error());
        assert_eq!(
            err.error(),
            format!("accept unix {socket_path}: use of closed network connection")
        );
        let dir = dir_of(&socket_path);
        match std::fs::metadata(&dir) {
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            other => panic!("socket directory still exists after close: {other:?}"),
        }
    }

    // PORT: no Go test. A NUL byte in the path is EINVAL in Go
    // (`syscall.BytePtrFromString`); std fails with no errno. Go N' text:
    // w2/l5-skeptic/probe/nul-go-Np.txt.
    #[test]
    fn nul_byte_in_pipe_path_is_einval() {
        let err = new_pipe_transport("a\0b").err().expect("a NUL byte fails");
        assert_eq!(
            err.error(),
            "create private socket directory: mkdir a\0b: invalid argument"
        );
    }

    // Go: ipc/transport_unix_test.go:47 TestPipeTransportPermissions
    // The umask is per process, so a helper process of this test binary
    // runs the test with umask 002.
    #[test]
    fn test_pipe_transport_permissions() {
        if std::env::var(PRIVATE_PIPE_HELPER_ENV).as_deref() == Ok("1") {
            test_pipe_transport_permissions_helper();
            return;
        }

        // `/proc/self/exe` still names this binary when a build replaces it.
        let proc_exe = std::path::Path::new("/proc/self/exe");
        let exe = if proc_exe.exists() {
            proc_exe.to_path_buf()
        } else {
            std::env::current_exe().expect("test binary")
        };
        // The child has copies of this process's descriptors until its exec
        // closes them, and the spawn can return before that, so the lock
        // stays until the child exits (`SPAWN_LOCK`).
        let spawn = crate::ipc::transport::SPAWN_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let output = std::process::Command::new(&exe)
            .args([
                "--exact",
                PERMISSIONS_TEST,
                "--nocapture",
                "--test-threads",
                "1",
            ])
            .env(PRIVATE_PIPE_HELPER_ENV, "1")
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run the helper process");
        drop(spawn);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "helper process failed: {}\n{stdout}{stderr}",
            output.status
        );
        // PORT: not in Go. A filter that matches no test also succeeds.
        assert!(stdout.contains("running 1 test"), "{stdout}{stderr}");
    }

    // Go: ipc/transport_unix_test.go:62 testPipeTransportPermissions
    fn test_pipe_transport_permissions_helper() {
        rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o002));

        let transport = new_private_pipe_transport("tsgo-api-test")
            .unwrap_or_else(|err| panic!("{}", err.error()));

        let socket_path = transport.path();
        assert_mode(&dir_of(&socket_path), 0o700);
        assert_mode(&socket_path, 0o600);

        let explicit_dir = path_join(&os_temp_dir(), &format!("tsgo-api-{}", std::process::id()));
        let explicit_transport =
            new_pipe_transport(&explicit_dir).unwrap_or_else(|err| panic!("{}", err.error()));
        assert_mode(&explicit_dir, 0o700);
        assert_mode(&path_join(&explicit_dir, "socket"), 0o600);

        assert!(
            new_pipe_transport(&explicit_dir).is_err(),
            "NewPipeTransport succeeded with an existing directory"
        );
        if let Err(err) = explicit_transport.close() {
            panic!("{}", err.error());
        }
        match std::fs::metadata(&explicit_dir) {
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            other => panic!("socket directory still exists after close: {other:?}"),
        }
        // Go: defer transport.Close()
        let _ = transport.close();
    }

    // Go: ipc/transport_unix_test.go:94 assertMode
    fn assert_mode(path: &str, want: u32) {
        let info = std::fs::metadata(path).unwrap_or_else(|err| panic!("{err}"));
        let got = info.permissions().mode() & 0o777;
        assert_eq!(got, want, "{path} mode = {got:04o}, want {want:04o}");
    }
}
