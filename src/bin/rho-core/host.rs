//! `rho-core serve`: holds one Core for a project so that runs outlive the
//! client invocations that submit, wait for, read and cancel them.

use std::fs;
use std::io::{self, BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rho_core::{
    Core, CoreConfig, Disposition, Executor, ForgetError, Limits, LookupError, OutputChunk,
    ReadError, RunRequest, RunView, Stream, SubmitError,
};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::wire::{self, Greeting, Line, Request, WireStream};

const DEFAULT_MAX_CONNECTIONS: usize = 16;
/// How long a stopping host lets open requests finish after stopping its runs.
const REQUEST_GRACE: Duration = Duration::from_secs(2);
const BUSY_WRITE_TIMEOUT: Duration = Duration::from_secs(1);
const ACCEPT_RETRY: Duration = Duration::from_millis(100);

pub fn main(args: Vec<String>) -> ExitCode {
    let options = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("rho-core serve: {message}\nRun `rho-core help` for usage.");
            return ExitCode::from(2);
        }
    };
    match serve(options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("rho-core serve: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Options {
    config: CoreConfig,
    max_connections: usize,
}

fn parse(args: Vec<String>) -> Result<Options, String> {
    let mut args = crate::Args::new(args);
    let (mut project, mut state_dir) = (None, None);
    let mut executors: Vec<Executor> = Vec::new();
    let mut limits = Limits::default();
    let mut max_connections = DEFAULT_MAX_CONNECTIONS;
    while let Some(flag) = args.next_flag()? {
        match flag.as_str() {
            "--project" => project = Some(args.value(&flag)?),
            "--state-dir" => state_dir = Some(args.value(&flag)?),
            "--executor" => {
                let (name, program) = pair(&flag, &args.value(&flag)?)?;
                executors.push(Executor::new(name, program));
            }
            "--executor-arg" => {
                let (name, arg) = pair(&flag, &args.value(&flag)?)?;
                let executor = executors
                    .iter_mut()
                    .find(|executor| executor.name == name)
                    .ok_or_else(|| format!("{flag} {name}: give --executor {name}=... first"))?;
                executor.args.push(arg);
            }
            "--limit" => {
                let (name, value) = pair(&flag, &args.value(&flag)?)?;
                set_limit(&mut limits, &name, &value)?;
            }
            "--max-connections" => {
                max_connections = crate::number(&flag, &args.value(&flag)?)?;
                if max_connections == 0 {
                    return Err("--max-connections must be at least 1".into());
                }
            }
            _ => return Err(format!("unknown option `{flag}`")),
        }
    }
    let project = project.ok_or("--project DIR is required")?;
    let state_dir = state_dir.ok_or("--state-dir DIR is required")?;
    if executors.is_empty() {
        return Err("at least one --executor NAME=/absolute/program is required".into());
    }
    let mut config = CoreConfig::new(project, state_dir).limits(limits);
    config.executors = executors;
    Ok(Options {
        config,
        max_connections,
    })
}

fn pair(flag: &str, value: &str) -> Result<(String, String), String> {
    value
        .split_once('=')
        .map(|(name, rest)| (name.to_owned(), rest.to_owned()))
        .ok_or_else(|| format!("{flag} expects NAME=VALUE, got `{value}`"))
}

fn set_limit(limits: &mut Limits, name: &str, value: &str) -> Result<(), String> {
    let flag = format!("--limit {name}");
    let count = || crate::number::<usize>(&flag, value);
    let millis = || crate::number::<u64>(&flag, value).map(Duration::from_millis);
    match name {
        "max_running" => limits.max_running = count()?,
        "max_records" => limits.max_records = count()?,
        "max_code_bytes" => limits.max_code_bytes = count()?,
        "max_args" => limits.max_args = count()?,
        "max_arg_bytes" => limits.max_arg_bytes = count()?,
        "max_stream_bytes" => limits.max_stream_bytes = crate::number(&flag, value)?,
        "max_output_chunks" => limits.max_output_chunks = count()?,
        "max_read_bytes" => limits.max_read_bytes = count()?,
        "stop_grace_ms" => limits.stop_grace = millis()?,
        "output_close_grace_ms" => limits.output_close_grace = millis()?,
        _ => {
            return Err(format!(
                "unknown limit `{name}`; known: max_running, max_records, max_code_bytes, \
                 max_args, max_arg_bytes, max_stream_bytes, max_output_chunks, max_read_bytes, \
                 stop_grace_ms, output_close_grace_ms"
            ));
        }
    }
    Ok(())
}

struct Host {
    core: Core,
    socket: PathBuf,
    max_connections: usize,
    max_request_bytes: usize,
    connections: AtomicUsize,
    stopping: AtomicBool,
}

fn serve(options: Options) -> Result<(), String> {
    // Before Core exists: a signal during startup stops the host once it is up.
    let signals = Signals::install()?;
    let core = Core::open(options.config).map_err(|e| e.to_string())?;
    // This host now holds the state dir's lock: no live host uses this socket path.
    let capability = core.capability();
    let socket = capability.state_dir.join(wire::SOCKET_NAME);
    let listener = listen(&socket)?;
    let bound = fs::metadata(&socket).map_err(|e| e.to_string())?;
    let host = Arc::new(Host {
        max_request_bytes: wire::max_request_bytes(&capability.limits),
        core,
        socket,
        max_connections: options.max_connections,
        connections: AtomicUsize::new(0),
        stopping: AtomicBool::new(false),
    });
    print_line(&json!({"ready": {
        "socket": host.socket,
        "project_root": capability.project_root,
        "state_dir": capability.state_dir,
        "pid": std::process::id(),
        "unreadable_records": host.core.unreadable_records().len(),
    }}));
    let acceptor = Arc::clone(&host);
    thread::Builder::new()
        .name("accept".into())
        .spawn(move || acceptor.accept(listener))
        .map_err(|e| format!("cannot start the accept thread: {e}"))?;

    // A failed signal read stops the host as a signal would; it is reported.
    let signal = signals.wait().map_err(|e| e.to_string());
    host.stopping.store(true, Ordering::SeqCst);
    // Wakes the accept thread, which then closes each new connection unanswered.
    drop(UnixStream::connect(&host.socket));
    if let Ok(current) = fs::symlink_metadata(&host.socket)
        && (current.dev(), current.ino()) == (bound.dev(), bound.ino())
    {
        let _ = fs::remove_file(&host.socket);
    }
    let report = host.core.shutdown();
    let deadline = Instant::now() + REQUEST_GRACE;
    while host.connections.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    print_line(&json!({"shutdown": {
        "signal": signal.as_ref().ok(),
        "signal_error": signal.as_ref().err(),
        "stopped": report.stopped,
        "not_confirmed": report.not_confirmed,
        "requests_still_open": host.connections.load(Ordering::SeqCst),
    }}));
    Ok(())
}

/// Replaces only a socket left by an earlier host of this state dir.
fn listen(socket: &Path) -> Result<UnixListener, String> {
    match fs::symlink_metadata(socket) {
        Ok(meta) if meta.file_type().is_socket() => fs::remove_file(socket)
            .map_err(|e| format!("cannot remove stale socket `{}`: {e}", socket.display()))?,
        Ok(_) => {
            return Err(format!(
                "`{}` exists and is not a socket; not replacing it",
                socket.display()
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot inspect `{}`: {e}", socket.display())),
    }
    let listener = UnixListener::bind(socket)
        .map_err(|e| format!("cannot listen on `{}`: {e}", socket.display()))?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot restrict `{}`: {e}", socket.display()))?;
    Ok(listener)
}

fn print_line(value: &Value) {
    let mut out = io::stdout().lock();
    let _ = writeln!(out, "{value}");
    let _ = out.flush();
}

/// A connection slot, released when the connection's thread ends.
struct Slot(Arc<Host>);

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Host {
    fn accept(self: Arc<Self>, listener: UnixListener) {
        for stream in listener.incoming() {
            if self.stopping.load(Ordering::SeqCst) {
                return;
            }
            match stream {
                Ok(stream) => self.admit(stream),
                Err(error) => {
                    eprintln!("rho-core serve: accept failed: {error}");
                    thread::sleep(ACCEPT_RETRY);
                }
            }
        }
    }

    fn admit(self: &Arc<Self>, stream: UnixStream) {
        let reserved = self
            .connections
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |open| {
                (open < self.max_connections).then_some(open + 1)
            });
        if reserved.is_err() {
            let _ = stream.set_write_timeout(Some(BUSY_WRITE_TIMEOUT));
            let _ = wire::send(
                &stream,
                &Greeting::Busy {
                    protocol: wire::PROTOCOL,
                    max_connections: self.max_connections,
                },
            );
            return;
        }
        let slot = Slot(Arc::clone(self));
        let host = Arc::clone(self);
        // On failure the closure, with its stream and slot, is dropped: the
        // client sees the connection close before a greeting.
        if let Err(error) = thread::Builder::new()
            .name("connection".into())
            .spawn(move || host.connection(stream, slot))
        {
            eprintln!("rho-core serve: cannot start a connection thread: {error}");
        }
    }

    fn connection(&self, stream: UnixStream, _slot: Slot) {
        let _ = stream.set_read_timeout(Some(wire::IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(wire::IO_TIMEOUT));
        if let Err(reason) = check_peer(&stream) {
            let refused = Greeting::Refused {
                protocol: wire::PROTOCOL,
                reason,
            };
            let _ = wire::send(&stream, &refused);
            return;
        }
        let ready = Greeting::Ready {
            protocol: wire::PROTOCOL,
            max_request_bytes: self.max_request_bytes,
        };
        if wire::send(&stream, &ready).is_err() {
            return;
        }
        let mut reader = BufReader::new(&stream);
        let refused = |error: Value| json!({"ok": false, "error": error});
        let reply = match wire::read_line(&mut reader, self.max_request_bytes) {
            Ok(Line::Complete(line)) => match serde_json::from_slice::<Request>(&line) {
                Ok(request) => self.call(request),
                Err(error) => refused(failure(
                    "bad_request",
                    format!("not a request this host understands ({error}); nothing was done"),
                )),
            },
            Ok(Line::TooLarge) => refused(failure(
                "request_too_large",
                format!(
                    "request exceeds {} bytes; nothing was done",
                    self.max_request_bytes
                ),
            )),
            // The client left or stalled before a whole request arrived.
            Ok(Line::Closed) | Err(_) => return,
        };
        let _ = wire::send(&stream, &reply);
    }

    fn call(&self, request: Request) -> Value {
        let core = &self.core;
        let result = match request {
            Request::Capability { contract } => Ok(self.capability(contract)),
            Request::Submit {
                caller,
                request_id,
                executor,
                workdir,
                code,
                args,
            } => {
                let request = RunRequest {
                    request_id,
                    executor,
                    workdir: workdir.into(),
                    code,
                    args,
                };
                core.submit(&caller, request)
                    .map(|submitted| {
                        let disposition = match submitted.disposition {
                            Disposition::Accepted => "accepted",
                            Disposition::Existing => "existing",
                        };
                        json!({"disposition": disposition, "run": run(&submitted.run)})
                    })
                    .map_err(submit_error)
            }
            Request::Lookup {
                caller,
                request_id,
                record,
            } => core
                .lookup(&caller, &request_id)
                .map(|view| {
                    let mut value = run(&view);
                    if record {
                        value["record"] = operation_record(&view);
                    }
                    value
                })
                .map_err(lookup_error),
            Request::List { caller } => core
                .list_operations(&caller)
                .map(|views| json!({"runs": views.iter().map(summary).collect::<Vec<_>>()}))
                .map_err(lookup_error),
            Request::Wait {
                caller,
                request_id,
                timeout_ms,
            } => {
                let timeout = Duration::from_millis(timeout_ms).min(wire::MAX_WAIT);
                core.wait(&caller, &request_id, timeout)
                    .map(|view| run(&view))
                    .map_err(lookup_error)
            }
            Request::Cancel { caller, request_id } => core
                .cancel(&caller, &request_id)
                .map(|view| run(&view))
                .map_err(lookup_error),
            Request::Read {
                caller,
                request_id,
                stream,
                offset,
                max_bytes,
            } => {
                let stream = match stream {
                    WireStream::Stdout => Stream::Stdout,
                    WireStream::Stderr => Stream::Stderr,
                };
                core.read_output(&caller, &request_id, stream, offset, max_bytes)
                    .map(chunk)
                    .map_err(read_error)
            }
            Request::Forget { caller, request_id } => core
                .forget(&caller, &request_id)
                .map(|()| json!({"forgotten": request_id}))
                .map_err(forget_error),
        };
        match result {
            Ok(value) => json!({"ok": true, "value": value}),
            Err(error) => json!({"ok": false, "error": error}),
        }
    }

    fn capability(&self, contract: bool) -> Value {
        let capability = self.core.capability();
        let limits = &capability.limits;
        let mut value = json!({
            "project_root": capability.project_root,
            "state_dir": capability.state_dir,
            "socket": self.socket,
            "executors": capability.executors.iter().map(|executor| json!({
                "name": executor.name,
                "program": executor.program,
                "args": executor.args,
            })).collect::<Vec<_>>(),
            "limits": {
                "max_running": limits.max_running,
                "max_records": limits.max_records,
                "max_code_bytes": limits.max_code_bytes,
                "max_args": limits.max_args,
                "max_arg_bytes": limits.max_arg_bytes,
                "max_stream_bytes": limits.max_stream_bytes,
                "max_output_chunks": limits.max_output_chunks,
                "max_read_bytes": limits.max_read_bytes,
                "stop_grace_ms": limits.stop_grace.as_millis() as u64,
                "output_close_grace_ms": limits.output_close_grace.as_millis() as u64,
            },
            "host": {
                "protocol": wire::PROTOCOL,
                "pid": std::process::id(),
                "max_connections": self.max_connections,
                "max_request_bytes": self.max_request_bytes,
                "max_wait_ms": wire::MAX_WAIT.as_millis() as u64,
                "io_timeout_ms": wire::IO_TIMEOUT.as_millis() as u64,
            },
            "unreadable_records": self.core.unreadable_records().into_iter()
                .map(|(path, reason)| json!({"path": path, "reason": reason}))
                .collect::<Vec<_>>(),
        });
        if contract {
            value["contract"] = json!(rho_core::CONTRACT);
        }
        value
    }
}

/// Only processes of this host's user may use it; the socket mode is 0600 too.
fn check_peer(stream: &UnixStream) -> Result<(), String> {
    // SAFETY: geteuid has no preconditions.
    let own = unsafe { libc::geteuid() };
    match peer_uid(stream) {
        Ok(uid) if uid == own => Ok(()),
        Ok(uid) => Err(format!("peer uid {uid} is not the host's uid {own}")),
        Err(error) => Err(format!("peer uid unavailable: {error}")),
    }
}

#[cfg(target_os = "macos")]
fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    let (mut uid, mut gid) = (0, 0);
    // SAFETY: a connected socket descriptor and two valid out-pointers.
    if unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(uid)
}

#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream) -> io::Result<libc::uid_t> {
    // SAFETY: zeroed ucred is a valid out-parameter of the size passed.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: a connected socket descriptor and a correctly sized buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(cred.uid)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn peer_uid(_stream: &UnixStream) -> io::Result<libc::uid_t> {
    Err(io::Error::other(
        "peer credentials are not implemented here",
    ))
}

/// Write end of the pipe that termination signals are reported on.
static SIGNAL_PIPE: AtomicI32 = AtomicI32::new(-1);

extern "C" fn report_signal(signal: libc::c_int) {
    let byte = signal as u8;
    // SAFETY: only async-signal-safe calls; errno is kept for the interrupted code.
    unsafe {
        let errno = errno();
        let saved = errno.map(|errno| *errno);
        libc::write(
            SIGNAL_PIPE.load(Ordering::Relaxed),
            (&byte as *const u8).cast(),
            1,
        );
        if let (Some(errno), Some(saved)) = (errno, saved) {
            *errno = saved;
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
unsafe fn errno() -> Option<*mut libc::c_int> {
    // SAFETY: returns this thread's errno location.
    Some(unsafe { libc::__error() })
}

#[cfg(any(target_os = "linux", target_os = "android"))]
unsafe fn errno() -> Option<*mut libc::c_int> {
    // SAFETY: returns this thread's errno location.
    Some(unsafe { libc::__errno_location() })
}

#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "linux",
    target_os = "android"
)))]
unsafe fn errno() -> Option<*mut libc::c_int> {
    None
}

/// SIGTERM, SIGINT and SIGHUP, reported by a handler as bytes on a pipe. No
/// signal mask changes: std passes this process's mask on to runs, and a run
/// with SIGTERM blocked could only be stopped by SIGKILL. Exec resets the
/// handler, and the pipe is close-on-exec, so runs start with default handling.
struct Signals(fs::File);

impl Signals {
    /// Signals inherited as ignored (as under `nohup`) stay ignored.
    fn install() -> Result<Self, String> {
        let failed = |what: &str| format!("cannot {what}: {}", io::Error::last_os_error());
        let mut fds = [0; 2];
        // SAFETY: a valid two-element array for the new descriptors.
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(failed("create the signal pipe"));
        }
        // SAFETY: pipe just returned these descriptors, owned by nothing else.
        let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        // SAFETY: fcntl on descriptors owned here.
        let flagged = unsafe {
            libc::fcntl(read.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && libc::fcntl(write.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) == 0
                && libc::fcntl(write.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) == 0
        };
        if !flagged {
            return Err(failed("configure the signal pipe"));
        }
        // Kept open for the life of the process; the handler may run at any time.
        SIGNAL_PIPE.store(write.into_raw_fd(), Ordering::SeqCst);
        for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
            // SAFETY: zeroed sigaction is a valid out-parameter and, once
            // filled in below, a valid handler description.
            unsafe {
                let mut current: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(signal, std::ptr::null(), &mut current) != 0 {
                    return Err(failed("read a signal disposition"));
                }
                if current.sa_sigaction == libc::SIG_IGN {
                    continue;
                }
                let mut action: libc::sigaction = std::mem::zeroed();
                action.sa_sigaction = report_signal as extern "C" fn(libc::c_int) as usize;
                action.sa_flags = libc::SA_RESTART;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(failed("install a signal handler"));
                }
            }
        }
        Ok(Self(fs::File::from(read)))
    }

    /// Blocks until a termination signal arrives and returns its number.
    fn wait(&self) -> io::Result<i32> {
        let mut byte = [0u8; 1];
        loop {
            match (&self.0).read(&mut byte) {
                Ok(1) => return Ok(i32::from(byte[0])),
                Ok(_) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
    }
}

fn run(view: &RunView) -> Value {
    let request = &view.request;
    times(json!({
        "request_id": request.request_id,
        "run_id": view.run_id,
        "terminal": view.is_terminal(),
        "held": view.held,
        "status": value(&view.status),
        "cancel": value(&view.cancel),
        "stdout": stream_info(&view.stdout),
        "stderr": stream_info(&view.stderr),
        "unsaved": view.unsaved,
        "retention": value(&view.operation.retention),
        "accepted_at": value(&view.accepted_at),
        "request": {
            "executor": request.executor,
            "workdir": request.workdir,
            "workdir_resolved": request.workdir_resolved,
            "args": request.args,
            "code_sha256": request.code_sha256,
            "code_bytes": request.code_bytes,
        },
    }))
}

fn summary(view: &RunView) -> Value {
    times(json!({
        "request_id": view.request.request_id,
        "run_id": view.run_id,
        "terminal": view.is_terminal(),
        "held": view.held,
        "status": value(&view.status),
        "unsaved": view.unsaved,
        "retention": value(&view.operation.retention),
        "accepted_at": value(&view.accepted_at),
    }))
}

/// The stored facts with their sources; output chunk lists become counts.
fn operation_record(view: &RunView) -> Value {
    let mut record = value(&view.operation);
    for stream in ["stdout", "stderr"] {
        if let Some(output) = record["outputs"][stream].as_object_mut()
            && let Some(blobs) = output.remove("blobs")
        {
            let count = blobs.as_array().map_or(0, Vec::len);
            output.insert("blob_count".into(), json!(count));
        }
    }
    times(record)
}

fn stream_info(info: &rho_core::StreamInfo) -> Value {
    let mut value = value(info);
    value["truncated"] = json!(info.truncated());
    value
}

fn chunk(chunk: OutputChunk) -> Value {
    let (text, used, lossy) = decode(&chunk.bytes);
    let next_offset = chunk.offset + used as u64;
    json!({
        "stream": match chunk.stream { Stream::Stdout => "stdout", Stream::Stderr => "stderr" },
        "offset": chunk.offset,
        "next_offset": next_offset,
        "text": text,
        "lossy": lossy,
        "more_retained": next_offset < chunk.info.retained_bytes,
        "info": stream_info(&chunk.info),
        "run_terminal": chunk.run_terminal,
    })
}

/// Text of `bytes` and how many bytes it covers. A UTF-8 sequence cut at the
/// end is left for the next read; other invalid bytes are replaced (`lossy`).
fn decode(bytes: &[u8]) -> (String, usize, bool) {
    match std::str::from_utf8(bytes) {
        Ok(text) => (text.to_owned(), bytes.len(), false),
        Err(error) if error.error_len().is_none() && error.valid_up_to() > 0 => {
            let valid = error.valid_up_to();
            let text = String::from_utf8_lossy(&bytes[..valid]).into_owned();
            (text, valid, false)
        }
        Err(_) => (
            String::from_utf8_lossy(bytes).into_owned(),
            bytes.len(),
            true,
        ),
    }
}

fn failure(kind: &str, message: impl Into<String>) -> Value {
    json!({"kind": kind, "message": message.into()})
}

fn submit_error(error: SubmitError) -> Value {
    let message = error.to_string();
    match error {
        SubmitError::InvalidRequest(_) => failure("invalid_request", message),
        SubmitError::UnknownExecutor(_) => failure("unknown_executor", message),
        SubmitError::Workdir { .. } => failure("workdir", message),
        SubmitError::Conflict { existing } => {
            let mut value = failure("conflict", message);
            value["existing"] = run(&existing);
            value
        }
        SubmitError::Capacity { resource, limit } => {
            let mut value = failure("capacity", message);
            value["resource"] = json!(resource);
            value["limit"] = json!(limit);
            value
        }
        SubmitError::ShuttingDown => failure("shutting_down", message),
        SubmitError::RecordUnreadable { .. } => failure("record_unreadable", message),
        SubmitError::Storage(_) => failure(
            "storage",
            format!(
                "{message}; whether the request was accepted is not known: look it up, or \
                 submit again with the same request id"
            ),
        ),
    }
}

fn lookup_error(error: LookupError) -> Value {
    let kind = match &error {
        LookupError::InvalidIdentity(_) => "invalid_identity",
        LookupError::NotFound { .. } => "not_found",
        LookupError::RecordUnreadable { .. } => "record_unreadable",
        LookupError::Storage(_) => "storage",
    };
    failure(kind, error.to_string())
}

fn read_error(error: ReadError) -> Value {
    match error {
        ReadError::Lookup(error) => lookup_error(error),
        ReadError::OffsetBeyondRetained { offset, retained } => {
            let mut value = failure("offset_beyond_retained", error.to_string());
            value["offset"] = json!(offset);
            value["retained"] = json!(retained);
            value
        }
        ReadError::Io(_) => failure("io", error.to_string()),
    }
}

fn forget_error(error: ForgetError) -> Value {
    let message = error.to_string();
    match error {
        ForgetError::Lookup(error) => lookup_error(error),
        ForgetError::NotTerminal(view) => {
            let mut value = failure("protected", message);
            value["run"] = run(&view);
            value
        }
        ForgetError::Io(_) => failure("io", message),
    }
}

fn value(fact: &impl Serialize) -> Value {
    serde_json::to_value(fact)
        .unwrap_or_else(|error| Value::String(format!("not serializable: {error}")))
}

/// Writes serialized `SystemTime`s as UTC RFC 3339 times with milliseconds.
fn times(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            if map.len() == 2
                && let (Some(secs), Some(nanos)) = (
                    map.get("secs_since_epoch").and_then(Value::as_u64),
                    map.get("nanos_since_epoch").and_then(Value::as_u64),
                )
            {
                let time = UNIX_EPOCH + Duration::new(secs, nanos as u32);
                return Value::String(rfc3339(time));
            }
            Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, times(v)))
                    .collect::<Map<_, _>>(),
            )
        }
        Value::Array(items) => Value::Array(items.into_iter().map(times).collect()),
        other => other,
    }
}

fn rfc3339(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (year, month, day) = civil_date((secs / 86_400) as i64);
    let rest = secs % 86_400;
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        since.subsec_millis()
    )
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (H. Hinnant's algorithm).
fn civil_date(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_times() {
        let at = |secs: u64, millis: u64| {
            rfc3339(UNIX_EPOCH + Duration::from_millis(secs * 1000 + millis))
        };
        assert_eq!(at(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(at(951_782_400, 5), "2000-02-29T00:00:00.005Z");
        assert_eq!(at(1_791_696_305, 123), "2026-10-11T05:25:05.123Z");
    }

    #[test]
    fn leaves_a_cut_utf8_sequence_for_the_next_read() {
        let bytes = "aé".as_bytes();
        assert_eq!(decode(&bytes[..2]), ("a".into(), 1, false));
        assert_eq!(decode(&bytes[1..2]), ("\u{fffd}".into(), 1, true));
        assert_eq!(decode(b"a\xffb"), ("a\u{fffd}b".into(), 3, true));
    }
}
