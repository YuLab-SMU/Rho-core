//! Durable run records: one directory per accepted identity under
//! `state_dir/records`, holding the request, the script snapshot, retained
//! output, a run lock and one small JSON file per known fact.
//!
//! A record is staged under `pending/` and committed by renaming the whole
//! directory into `records/`; nothing is dispatched before that rename. Facts are
//! written to a temporary file, synced, renamed into place and the directory synced.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::fault;
use crate::run::{CancelInfo, RequestInfo, RunStatus, StreamInfo};

pub(crate) const RECORDS: &str = "records";
pub(crate) const PENDING: &str = "pending";
pub(crate) const TRASH: &str = "trash";
pub(crate) const STORE_LOCK: &str = "store.lock";

pub(crate) const REQUEST: &str = "request.json";
pub(crate) const DISPATCH: &str = "dispatch.json";
pub(crate) const SPAWNED: &str = "spawned.json";
pub(crate) const CANCEL: &str = "cancel.json";
pub(crate) const OUTCOME: &str = "outcome.json";
pub(crate) const SCRIPT: &str = "script";
pub(crate) const RUN_LOCK: &str = "run.lock";
pub(crate) const STDOUT: &str = "stdout";
pub(crate) const STDERR: &str = "stderr";
const STORE_INFO: &str = "store.json";
const LOCK_PATIENCE: Duration = Duration::from_millis(500);

/// Written before dispatch; never changed.
#[derive(Serialize, Deserialize)]
pub(crate) struct Accepted {
    pub(crate) run_id: String,
    pub(crate) fingerprint: String,
    pub(crate) request: RequestInfo,
    pub(crate) accepted_at: SystemTime,
    pub(crate) stream_limit: u64,
}

/// Written immediately before the process is started.
#[derive(Serialize, Deserialize)]
pub(crate) struct Dispatch {
    pub(crate) at: SystemTime,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Spawned {
    pub(crate) pid: u32,
    pub(crate) spawned_at: SystemTime,
}

/// The terminal facts observed by the holder of the run.
#[derive(Serialize, Deserialize)]
pub(crate) struct Outcome {
    pub(crate) status: RunStatus,
    pub(crate) cancel: Option<CancelInfo>,
    pub(crate) stdout: StreamInfo,
    pub(crate) stderr: StreamInfo,
}

/// Directory name of an identity's record. Identities are case-sensitive and
/// may be `.` or `..`, so they are hashed rather than used as path components.
pub(crate) fn record_key(caller: &str, request_id: &str) -> String {
    let mut hasher = Sha256::new();
    for part in [
        b"rho-core/record/v1".as_slice(),
        caller.as_bytes(),
        request_id.as_bytes(),
    ] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part);
    }
    crate::hex(&hasher.finalize())
}

pub(crate) fn is_record_key(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

pub(crate) fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Takes a non-blocking exclusive lock on `file`; `false` if another open
/// description holds it.
pub(crate) fn try_lock(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

#[derive(Serialize, Deserialize)]
struct StoreInfo {
    project_root: PathBuf,
}

/// Opens `state_dir` exclusively for one Core and binds it to one project.
/// Uncommitted staging and removed records left by an earlier instance are
/// discarded: nothing in them was dispatched or is still referenced.
pub(crate) fn open(state_dir: &Path, project_root: &Path) -> Result<(PathBuf, File), String> {
    let at = |e: io::Error| format!("state dir `{}`: {e}", state_dir.display());
    fs::create_dir_all(state_dir).map_err(at)?;
    let state_dir = fs::canonicalize(state_dir).map_err(at)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(state_dir.join(STORE_LOCK))
        .map_err(at)?;
    // A process forked by another thread holds a copy of the lock's open file
    // description until it execs, so contention is retried for a short,
    // bounded time before the dir is reported as in use.
    let deadline = Instant::now() + LOCK_PATIENCE;
    while !try_lock(&lock).map_err(at)? {
        if Instant::now() >= deadline {
            return Err(format!(
                "state dir `{}` is in use by another Core",
                state_dir.display()
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    match read_fact::<StoreInfo>(&state_dir, STORE_INFO)? {
        Some(info) if info.project_root != project_root => {
            return Err(format!(
                "state dir `{}` belongs to project `{}`",
                state_dir.display(),
                info.project_root.display()
            ));
        }
        Some(_) => {}
        None => write_fact(
            &state_dir,
            STORE_INFO,
            &StoreInfo {
                project_root: project_root.to_owned(),
            },
        )
        .map_err(at)?,
    }
    for name in [RECORDS, PENDING, TRASH] {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_dir.join(name))
            .map_err(at)?;
    }
    for name in [PENDING, TRASH] {
        for entry in fs::read_dir(state_dir.join(name)).map_err(at)? {
            let _ = fs::remove_dir_all(entry.map_err(at)?.path());
        }
    }
    Ok((state_dir, lock))
}

/// Record directories in the store, by key. A listing failure is an error,
/// never an empty store.
pub(crate) fn list(state_dir: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    let mut records = Vec::new();
    for entry in fs::read_dir(state_dir.join(RECORDS))? {
        let entry = entry?;
        if let Some(name) = entry.file_name().to_str().filter(|n| is_record_key(n)) {
            records.push((name.to_owned(), entry.path()));
        }
    }
    Ok(records)
}

/// Stages a record and commits it by renaming it into `records/`. On error
/// nothing is committed and nothing may be dispatched.
pub(crate) fn commit(
    state_dir: &Path,
    key: &str,
    record: &Accepted,
    code: &str,
) -> io::Result<PathBuf> {
    let staging = state_dir.join(PENDING).join(&record.run_id);
    let target = state_dir.join(RECORDS).join(key);
    let staged = (|| {
        fs::DirBuilder::new().mode(0o700).create(&staging)?;
        let mut script = create_new(&staging.join(SCRIPT))?;
        script.write_all(code.as_bytes())?;
        script.sync_all()?;
        for name in [STDOUT, STDERR, RUN_LOCK] {
            create_new(&staging.join(name))?.sync_all()?;
        }
        write_fact(&staging, REQUEST, record)?;
        fault::point("staged");
        if target.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("record `{}` exists but is not indexed", target.display()),
            ));
        }
        fs::rename(&staging, &target)
    })();
    if let Err(error) = staged {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    if let Err(error) = sync_dir(&state_dir.join(RECORDS)) {
        // Not known to be durable: withdraw it rather than dispatch it.
        if fs::rename(&target, &staging).is_ok() {
            let _ = fs::remove_dir_all(&staging);
        }
        return Err(error);
    }
    Ok(target)
}

/// Removes a record: renamed out of `records/` first, so an interrupted removal
/// never leaves a half-deleted record that reads as accepted.
pub(crate) fn discard(state_dir: &Path, dir: &Path, run_id: &str) -> io::Result<()> {
    let trash = state_dir.join(TRASH).join(run_id);
    fs::rename(dir, &trash)?;
    // If this sync is lost the record reappears after an OS crash, which keeps
    // deduplication rather than allowing a replay.
    let _ = sync_dir(&state_dir.join(RECORDS));
    let _ = fs::remove_dir_all(&trash);
    Ok(())
}

fn create_new(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
}

/// Replaces `dir/name` with `value` durably.
pub(crate) fn write_fact<T: Serialize>(dir: &Path, name: &str, value: &T) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    let temporary = dir.join(format!("{name}.tmp"));
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    fs::rename(&temporary, dir.join(name))?;
    sync_dir(dir)
}

/// Reads `dir/name`; `None` only if the file does not exist.
pub(crate) fn read_fact<T: DeserializeOwned>(dir: &Path, name: &str) -> Result<Option<T>, String> {
    match fs::read(dir.join(name)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| format!("`{name}` is not a readable record: {error}")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("`{name}` could not be read: {error}")),
    }
}
