//! Windows dispatch and holding of one native run.
//!
//! A run is held by a named Job Object (`Local\<boot>.<run_id>`) that contains
//! the executor and everything it starts, so grandchildren stop with their
//! parent the way a Unix process group would. The job is created without
//! KILL_ON_JOB_CLOSE: if the Core host dies, its processes keep running and a
//! later Core reopens the job by name to observe release. The holder thread
//! is the only place that stops the job, and it never stops it after
//! observing the main process exit. Output pipes are read in non-blocking
//! slices inside the holder loop; Windows pipes have no poll, so the loop
//! cadence is the readiness bound and the output-close grace is the drain
//! bound.

use std::fs::{self, File};
use std::io::{self, Read};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JobObjectExtendedLimitInformation, SetInformationJobObject,
};

use super::super::{
    Exit, Limits, POLL_INTERVAL, RunShared, Stream, StreamInfo, empty_stream, lock,
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
    let prepared = (|| -> Result<(File, String), String> {
        // The run dir keeps the lock file; mode bits have no Windows meaning.
        fs::create_dir(&run.dir).map_err(|e| e.to_string())?;
        let lock_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(run.dir.join(store::RUN_LOCK))
            .map_err(|e| e.to_string())?;
        if !store::try_lock(&lock_file).map_err(|e| e.to_string())? {
            return Err("another process holds this run's lock".into());
        }
        let script = lock(&run.state).record.script.clone();
        run.store.verify_blob(&script).map_err(|e| e.to_string())?;
        run.begin_dispatch()?;
        let boot = match os::process_context() {
            Ok(context) => context.boot_id,
            Err(error) => return Err(format!("boot session: {error}")),
        };
        Ok((lock_file, os::job_name(&boot, &run.run_id)))
    })();
    let (lock_file, job_name) = match prepared {
        Ok(prepared) => prepared,
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
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return run.not_started(format!("process could not be started: {error}"), running);
        }
    };
    // The job is created after the spawn and before the spawn fact: a crash
    // between them leaves a detached process no Core holds, the same window
    // the Unix dispatch has between fork and setpgid.
    let job = match create_job(&job_name, &child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return run.not_started(
                format!("run job could not hold the process: {error}"),
                running,
            );
        }
    };
    let pid = child.id();
    let spawned_at = run.set_running(pid);
    let stdout = child
        .stdout
        .take()
        .map(|pipe| File::from(OwnedHandle::from(pipe)));
    let pumps = [
        Pump::start(
            stdout,
            Arc::clone(run),
            Stream::Stdout,
            limits.max_stream_bytes,
        ),
        Pump::from_stderr(
            child.stderr.take(),
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
        job_name: job_name.clone(),
        job,
    };
    let held = thread::Builder::new()
        .name(format!("rho-run-{}", run.run_id))
        .spawn(move || holder.hold());
    if let Err(error) = held {
        // No holder thread exists: stop the run's job and record the outcome.
        let exit = match stop_job(&job_name) {
            Ok(()) => Exit::Signal(9),
            Err(error) => Exit::Unknown(format!("stopping without a holder failed: {error}")),
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
        run.finish(true, running);
    }
}

/// Creates the run's job, assigns the child to it and keeps a handle alive.
fn create_job(name: &str, child: &Child) -> io::Result<OwnedHandle> {
    let wide = os::to_wide(name);
    // SAFETY: a NUL-terminated name and default attributes.
    let job = unsafe { CreateJobObjectW(std::ptr::null(), wide.as_ptr()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // No silent breakaway: children stay in the job the way Unix children
    // stay in the process group. A process that explicitly requests
    // CREATE_BREAKAWAY_FROM_JOB fails to spawn and the run reports NotStarted,
    // the same explicitness a Unix setsid escape would show.
    info.BasicLimitInformation.LimitFlags = 0;
    // SAFETY: a valid job handle and a correctly sized info structure.
    let ok = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        // SAFETY: a handle CreateJobObjectW returned.
        unsafe { CloseHandle(job) };
        return Err(io::Error::last_os_error());
    }
    // SAFETY: a live child's process handle.
    let assigned = unsafe { AssignProcessToJobObject(job, child.as_raw_handle() as HANDLE) };
    if assigned == 0 {
        // SAFETY: a handle CreateJobObjectW returned.
        unsafe { CloseHandle(job) };
        return Err(io::Error::last_os_error());
    }
    // SAFETY: taking ownership of the job handle this function owns.
    Ok(unsafe { OwnedHandle::from_raw_handle(job as RawHandle) })
}

/// Terminates every process in the named job.
fn stop_job(name: &str) -> io::Result<()> {
    os::terminate_job(name)
}

/// Owns the child process: the only place that stops or waits on it.
struct Holder {
    run: Arc<RunShared>,
    child: Child,
    pid: u32,
    spawned_at: SystemTime,
    pumps: [Pump; 2],
    limits: Limits,
    running: Arc<AtomicUsize>,
    /// Shared with the process; releasing this is not a native exit fact.
    lock: Option<File>,
    job_name: String,
    /// The run's job object; held for the holder's lifetime. Named jobs die
    /// with their last handle even while processes remain assigned, and MSYS
    /// children break away silently when allowed, so the handle must stay.
    #[allow(dead_code)]
    job: OwnedHandle,
}

impl Holder {
    fn hold(mut self) {
        let mut term_sent: Option<Instant> = None;
        let mut kill_sent = false;
        let mut term_failed: Option<bool> = None;
        let mut close_deadline: Option<Instant> = None;
        let mut group_released = false;
        loop {
            self.drain_pumps();
            if let Some(deadline) = close_deadline {
                group_released = group_released || !os::job_has_processes(&self.job_name);
                let closed = self.pumps.iter().all(Pump::closed);
                if (closed && group_released) || Instant::now() >= deadline {
                    break;
                }
                thread::sleep(POLL_INTERVAL);
                continue;
            }
            if self.run.cancel_requested() {
                match term_sent {
                    None => {
                        // The first stop can fail deterministically under the
                        // signal-term fault point, the way a Unix SIGTERM
                        // stop can; the second stop then escalates.
                        let result = if term_failed.is_none() {
                            // The signal-term fault point makes this first
                            // stop fail deterministically under experiments.
                            let result = crate::fault::io_failure("signal-term")
                                .map_or_else(|| stop_job(&self.job_name), Err);
                            term_failed = Some(result.is_err());
                            result
                        } else {
                            stop_job(&self.job_name)
                        };
                        self.run.note_signal(false, result);
                        term_sent = Some(Instant::now());
                    }
                    Some(at) if !kill_sent && at.elapsed() >= self.limits.stop_grace => {
                        // The second stop re-issues the terminate: a failed
                        // first stop (a fault point or a transient error)
                        // must not leave the job running, the way a Unix
                        // holder escalates from SIGTERM to SIGKILL.
                        let result = stop_job(&self.job_name);
                        self.run.note_signal(true, result);
                        kill_sent = true;
                    }
                    Some(_) => {}
                }
            }
            if let Some(exit) = self.observe_exit() {
                self.run.observe_exit(self.pid, self.spawned_at, exit);
                close_deadline = Some(Instant::now() + self.limits.output_close_grace);
                continue;
            }
            if close_deadline.is_none() {
                thread::sleep(
                    POLL_INTERVAL
                        .min(self.limits.stop_grace)
                        .max(Duration::from_millis(1)),
                );
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
                // The reader thread ends with the process or pipe close.
                if let Some(reader) = pump.reader.take() {
                    let _ = reader.join();
                }
            }
        }
        drop(self.lock.take());
        drop(self.job);
        self.run.finish(group_released, &self.running);
    }

    /// Observes the main process's exit; on exit, stops the rest of the job
    /// before reporting, then lets the pumps drain to their grace.
    fn observe_exit(&mut self) -> Option<Exit> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                // The job's other members end now; the group is not released
                // until the job is observed empty.
                let _ = stop_job(&self.job_name);
                Some(exit_of(status))
            }
            Ok(None) => None,
            Err(error) => Some(Exit::Unknown(error.to_string())),
        }
    }

    /// Commits whatever each stream's reader has forwarded.
    fn drain_pumps(&mut self) {
        for pump in &mut self.pumps {
            pump.read_once();
        }
    }
}

fn exit_of(status: ExitStatus) -> Exit {
    match status.code() {
        Some(code) => Exit::Code(code),
        // An abnormal termination is a job stop, reported as a signal would be.
        None => Exit::Signal(15),
    }
}

/// Drains one native pipe on its own thread. A blocking pipe read would stop
/// the holder loop from acting on cancels, so each stream gets a reader that
/// forwards bytes through a channel; the holder polls both channels.
struct Pump {
    events: std::sync::mpsc::Receiver<PumpEvent>,
    /// Set once the reader ended; no more events arrive afterwards.
    reader: Option<thread::JoinHandle<()>>,
    run: Arc<RunShared>,
    stream: Stream,
    info: StreamInfo,
}

/// One read outcome from a pump's reader thread.
enum PumpEvent {
    Bytes(Vec<u8>),
    Eof,
    Failed(String),
}

impl Pump {
    fn start(pipe: Option<File>, run: Arc<RunShared>, stream: Stream, limit: u64) -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        let reader = pipe.map(|mut pipe| {
            let sender = sender.clone();
            thread::Builder::new()
                .name(format!("rho-pump-{}", stream.kind()))
                .spawn(move || {
                    loop {
                        let mut buffer = [0u8; 8192];
                        match pipe.read(&mut buffer) {
                            Ok(0) => {
                                let _ = sender.send(PumpEvent::Eof);
                                break;
                            }
                            Ok(read) => {
                                if sender
                                    .send(PumpEvent::Bytes(buffer[..read].to_vec()))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(error)
                                if matches!(
                                    error.kind(),
                                    io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                                ) =>
                            {
                                continue;
                            }
                            Err(error) => {
                                let _ = sender.send(PumpEvent::Failed(error.to_string()));
                                break;
                            }
                        }
                    }
                })
                .ok()
        });
        Self {
            events: receiver,
            reader: reader.expect("pump reader always spawns"),
            run,
            stream,
            info: empty_stream(limit),
        }
    }

    fn from_stderr(
        pipe: Option<std::process::ChildStderr>,
        run: Arc<RunShared>,
        stream: Stream,
        limit: u64,
    ) -> Self {
        let file = pipe.map(|pipe| File::from(OwnedHandle::from(pipe)));
        Self::start(file, run, stream, limit)
    }

    fn closed(&self) -> bool {
        self.reader.is_none()
    }

    /// Drains pending events and commits them as output facts.
    fn read_once(&mut self) -> bool {
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            match event {
                PumpEvent::Bytes(bytes) => {
                    self.info = self.run.record_output(self.stream, &bytes, false, None);
                }
                PumpEvent::Eof => {
                    self.info = self.run.record_output(self.stream, &[], true, None);
                    self.reader = None;
                }
                PumpEvent::Failed(error) => {
                    self.info = self.run.record_output(
                        self.stream,
                        &[],
                        false,
                        Some(format!("reading output failed: {error}")),
                    );
                    self.reader = None;
                }
            }
        }
        changed
    }
}
