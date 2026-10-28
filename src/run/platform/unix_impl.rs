//! Unix dispatch and holding of one native run.
//!
//! The holder thread is the only place that signals or reaps the process. The
//! process group id is the main pid; the group is released when no member
//! remains. Output pipes are polled with `poll(2)`.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::super::{
    Exit, Limits, POLL_INTERVAL, READ_BUFFER, RunShared, Stream, empty_stream, lock,
};
use crate::store;
use crate::{Executor, fault, os};

pub(crate) fn dispatch(
    run: &Arc<RunShared>,
    executor: &Executor,
    args: &[String],
    limits: &Limits,
    running: &Arc<AtomicUsize>,
) {
    let prepared = (|| -> Result<File, String> {
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&run.dir)
            .map_err(|e| e.to_string())?;
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(run.dir.join(store::RUN_LOCK))
            .map_err(|e| e.to_string())?;
        if !store::try_lock(&lock_file).map_err(|e| e.to_string())? {
            return Err("another process holds this run's lock".into());
        }
        let script = lock(&run.state).record.script.clone();
        run.store.verify_blob(&script).map_err(|e| e.to_string())?;
        run.begin_dispatch()?;
        Ok(lock_file)
    })();
    let lock_file = match prepared {
        Ok(file) => file,
        Err(error) => {
            return run.not_started(format!("run files could not be prepared: {error}"), running);
        }
    };
    fault::point("dispatched");
    if run.cancel_requested() {
        return run.not_started(
            "cancel was requested before the process started".into(),
            running,
        );
    }
    let stdin = match lock_file.try_clone() {
        Ok(file) => file,
        Err(error) => {
            return run.not_started(format!("run lock could not be shared: {error}"), running);
        }
    };
    let script = lock(&run.state).record.script.clone();
    let script_path = match run.store.blob_path(&script) {
        Ok(path) => path,
        Err(error) => {
            return run.not_started(
                format!("script snapshot could not be resolved: {error}"),
                running,
            );
        }
    };
    let mut command = Command::new(&run.request.program);
    command
        .args(&executor.args)
        .arg(script_path)
        .args(args)
        .current_dir(&run.request.workdir_resolved)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return run.not_started(format!("process could not be started: {error}"), running);
        }
    };
    let pid = child.id();
    let spawned_at = run.set_running(pid);
    let pumps = [
        Pump::new(
            child.stdout.take().map(OwnedFd::from),
            Arc::clone(run),
            Stream::Stdout,
            limits.max_stream_bytes,
        ),
        Pump::new(
            child.stderr.take().map(OwnedFd::from),
            Arc::clone(run),
            Stream::Stderr,
            limits.max_stream_bytes,
        ),
    ];
    let holder = Holder {
        run: Arc::clone(run),
        child,
        pid,
        spawned_at,
        pumps,
        limits: limits.clone(),
        running: Arc::clone(running),
        lock: Some(lock_file),
    };
    let held = thread::Builder::new()
        .name(format!("rho-run-{}", run.run_id))
        .spawn(move || holder.hold());
    if let Err(error) = held {
        let pgid = pid as i32;
        let _ = os::signal_group(pgid, libc::SIGKILL);
        let exit = match os::reap(pgid) {
            Ok(raw) => exit_of(ExitStatus::from_raw(raw)),
            Err(error) => Exit::Unknown(error.to_string()),
        };
        run.observe_exit(pid, spawned_at, exit);
        for stream in [Stream::Stdout, Stream::Stderr] {
            run.record_output(
                stream,
                &[],
                false,
                Some(format!("output not retained: no holder thread: {error}")),
            );
        }
        run.finish(!os::group_exists(pgid), running);
    }
}
fn exit_of(status: ExitStatus) -> Exit {
    match (status.code(), status.signal()) {
        (Some(code), _) => Exit::Code(code),
        (None, Some(signal)) => Exit::Signal(signal),
        _ => Exit::Unknown(status.to_string()),
    }
}

/// Owns the child process: the only place that signals or reaps it.
struct Holder {
    run: Arc<RunShared>,
    child: Child,
    pid: u32,
    spawned_at: SystemTime,
    pumps: [Pump; 2],
    limits: Limits,
    running: Arc<AtomicUsize>,
    /// Shared with the process group; releasing this is not a native exit fact.
    lock: Option<File>,
}

impl Holder {
    fn hold(mut self) {
        let pgid = self.pid as i32;
        let mut buffer = vec![0; READ_BUFFER];
        let mut term_sent: Option<Instant> = None;
        let mut kill_sent = false;
        let mut close_deadline: Option<Instant> = None;
        let mut group_released = false;
        loop {
            let timeout = close_deadline.map_or(POLL_INTERVAL, |deadline| {
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(POLL_INTERVAL)
            });
            self.pump(&mut buffer, timeout);
            if let Some(deadline) = close_deadline {
                group_released = group_released || !os::group_exists(pgid);
                let closed = self.pumps.iter().all(Pump::closed);
                if (closed && group_released) || Instant::now() >= deadline {
                    break;
                }
                continue;
            }
            if self.run.cancel_requested() {
                match term_sent {
                    None => {
                        let result = os::signal_group(pgid, libc::SIGTERM);
                        self.run.note_signal(false, result);
                        term_sent = Some(Instant::now());
                    }
                    Some(at) if !kill_sent && at.elapsed() >= self.limits.stop_grace => {
                        let result = os::signal_group(pgid, libc::SIGKILL);
                        self.run.note_signal(true, result);
                        kill_sent = true;
                    }
                    Some(_) => {}
                }
            }
            if let Some(exit) = self.observe_exit() {
                self.run.observe_exit(self.pid, self.spawned_at, exit);
                close_deadline = Some(Instant::now() + self.limits.output_close_grace);
            }
        }
        for pump in &mut self.pumps {
            if !pump.closed() {
                pump.info = self.run.record_output(
                    pump.stream,
                    &[],
                    false,
                    Some("output close grace exceeded; later bytes are not retained".into()),
                );
                pump.pipe = None;
            }
        }
        drop(self.lock.take());
        self.run.finish(group_released, &self.running);
    }

    /// Observes the main process's exit; if it exited, releases the rest of its
    /// process group while the unreaped leader still reserves the group id, then reaps.
    fn observe_exit(&mut self) -> Option<Exit> {
        let pgid = self.pid as i32;
        match os::has_exited(pgid) {
            Ok(false) => None,
            Ok(true) => {
                let _ = os::signal_group(pgid, libc::SIGKILL);
                Some(match self.child.wait() {
                    Ok(status) => exit_of(status),
                    Err(error) => Exit::Unknown(error.to_string()),
                })
            }
            Err(_) => match self.child.try_wait() {
                Ok(Some(status)) => Some(exit_of(status)),
                Ok(None) => None,
                Err(error) => Some(Exit::Unknown(error.to_string())),
            },
        }
    }

    /// Reads whatever output is ready, waiting at most `timeout`. Returns
    /// whether stream state changed.
    fn pump(&mut self, buffer: &mut [u8], timeout: Duration) -> bool {
        let open: Vec<usize> = (0..self.pumps.len())
            .filter(|&index| !self.pumps[index].closed())
            .collect();
        let fds: Vec<RawFd> = open
            .iter()
            .filter_map(|&index| self.pumps[index].fd())
            .collect();
        let ready = match os::poll_readable(&fds, timeout) {
            Ok(ready) => ready,
            Err(_) => {
                thread::sleep(timeout);
                return false;
            }
        };
        let mut changed = false;
        for (slot, &index) in open.iter().enumerate() {
            if ready[slot] {
                changed |= self.pumps[index].read_once(buffer);
            }
        }
        changed
    }
}

/// Drains one native pipe. Retained bytes and counters are committed separately
/// from the exit, cancellation and release dimensions.
struct Pump {
    pipe: Option<File>,
    run: Arc<RunShared>,
    stream: Stream,
    info: StreamInfo,
}

impl Pump {
    fn new(pipe: Option<OwnedFd>, run: Arc<RunShared>, stream: Stream, limit: u64) -> Self {
        Self {
            pipe: pipe.map(File::from),
            run,
            stream,
            info: empty_stream(limit),
        }
    }
    fn closed(&self) -> bool {
        self.pipe.is_none()
    }
    fn fd(&self) -> Option<RawFd> {
        self.pipe.as_ref().map(AsRawFd::as_raw_fd)
    }
    fn read_once(&mut self, buffer: &mut [u8]) -> bool {
        let Some(pipe) = self.pipe.as_mut() else {
            return false;
        };
        match pipe.read(buffer) {
            Ok(0) => {
                self.pipe = None;
                self.info = self.run.record_output(self.stream, &[], true, None);
            }
            Ok(read) => {
                self.info = self
                    .run
                    .record_output(self.stream, &buffer[..read], false, None);
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                ) =>
            {
                return false;
            }
            Err(error) => {
                self.pipe = None;
                self.info = self.run.record_output(
                    self.stream,
                    &[],
                    false,
                    Some(format!("reading output failed: {error}")),
                );
            }
        }
        true
    }
}
