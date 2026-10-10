//! SQLite owns acceptance and each independent fact transaction. Immutable,
//! bounded blobs are synced before a transaction references them.

use std::collections::HashSet;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
#[cfg(target_os = "macos")]
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::record::*;
use crate::run::lock;
use crate::{RequestInfo, StreamInfo, fault};

pub(crate) const DATABASE: &str = "core.sqlite3";
pub(crate) const RUN_LOCK: &str = "run.lock";
const STORE_LOCK: &str = "store.lock";
const FORMAT: i64 = 1;
const LOCK_PATIENCE: Duration = Duration::from_millis(500);
pub(crate) const BLOB_CHUNK: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) enum StoreError {
    Storage(String),
    Unreadable(String),
    Capacity(usize),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(s) | Self::Unreadable(s) => f.write_str(s),
            Self::Capacity(n) => write!(f, "limit of {n} accepted request records reached"),
        }
    }
}
impl From<rusqlite::Error> for StoreError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
impl From<io::Error> for StoreError {
    fn from(e: io::Error) -> Self {
        Self::Storage(e.to_string())
    }
}
impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Unreadable(e.to_string())
    }
}
type Result<T> = std::result::Result<T, StoreError>;

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Accepted {
    pub run_id: String,
    pub fingerprint: String,
    pub key: RequestKey,
    pub request: RequestInfo,
    pub script: BlobRef,
    pub accepted_at: SystemTime,
    pub stream_limit: u64,
    pub chunk_limit: usize,
}

pub(crate) struct Stored {
    pub accepted: Accepted,
    pub record: OperationRecord,
}

#[derive(Serialize, Deserialize)]
struct OutputMetadata {
    info: StreamInfo,
    observed_at: SystemTime,
    source: FactSource,
}

impl From<&OutputStream> for OutputMetadata {
    fn from(output: &OutputStream) -> Self {
        Self {
            info: output.info.clone(),
            observed_at: output.observed_at,
            source: output.source,
        }
    }
}

pub(crate) enum Acceptance {
    New(Box<Stored>),
    Existing(Box<Stored>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fact {
    Dispatch,
    Execution,
    Cancel,
    Stdout,
    Stderr,
    Release,
}
impl Fact {
    pub fn column(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::Execution => "execution",
            Self::Cancel => "cancellation",
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
            Self::Release => "release",
        }
    }
}

struct Inner {
    db: Connection,
    // Blobs needed by unsaved facts remain readable and cannot be collected by
    // another operation's forget. A successful fact commit removes its pins.
    pins: HashSet<(String, String, String)>,
}

struct OwnerLock(File);
impl Drop for OwnerLock {
    fn drop(&mut self) {
        // Explicit unlock also releases copies temporarily inherited across a
        // concurrent fork, rather than waiting for every copied fd to close.
        let _ = self.0.unlock();
    }
}

pub(crate) struct OperationStore {
    pub dir: PathBuf,
    pub scope_id: String,
    inner: Mutex<Inner>,
    _owner: OwnerLock,
}

fn json<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|e| StoreError::Storage(format!("fact could not be serialized: {e}")))
}
fn parse<T: serde::de::DeserializeOwned>(name: &str, value: &str) -> Result<T> {
    serde_json::from_str(value).map_err(|e| StoreError::Unreadable(format!("{name}: {e}")))
}

pub(crate) fn try_lock(file: &File) -> io::Result<bool> {
    match file.try_lock() {
        Ok(()) => Ok(true),
        Err(fs::TryLockError::WouldBlock) => Ok(false),
        Err(fs::TryLockError::Error(e)) => Err(e),
    }
}

fn mkdir(path: &Path) -> io::Result<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        return Err(io::Error::other("store directory is not a real directory"));
    }
    Ok(())
}

pub(crate) fn sync_file(file: &File) -> io::Result<()> {
    file.sync_all()?;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a valid open descriptor; F_FULLFSYNC takes no pointer.
        if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}
fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

impl OperationStore {
    pub fn open(state_dir: &Path, project_root: &Path) -> Result<Self> {
        mkdir(state_dir)?;
        let dir = fs::canonicalize(state_dir)?;
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(dir.join(STORE_LOCK))?;
        let deadline = Instant::now() + LOCK_PATIENCE;
        while !try_lock(&lock_file)? {
            if Instant::now() >= deadline {
                return Err(StoreError::Storage(
                    "state dir is in use by another Core".into(),
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
        let owner = OwnerLock(lock_file);
        // An unimported old store must never look like an empty new one. This
        // rebuild deliberately uses fresh stores rather than migrating data.
        if dir.join("store.json").exists() || dir.join("records").exists() {
            return Err(StoreError::Unreadable(
                "legacy file store: use a fresh state_dir; automatic import is not supported"
                    .into(),
            ));
        }
        let path = dir.join(DATABASE);
        let existed = path.try_exists()?;
        let db_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)?;
        // Existing empty/truncated files are damage, not permission to initialize.
        if existed && db_file.metadata()?.len() == 0 {
            return Err(StoreError::Unreadable(
                "existing database is empty or truncated".into(),
            ));
        }
        let mut db = Connection::open(&path)?;
        db.busy_timeout(Duration::from_millis(500))?;
        db.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON; PRAGMA wal_autocheckpoint=256;")?;
        let mode: String = db.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
        if mode != "wal" {
            return Err(StoreError::Storage("WAL could not be enabled".into()));
        }
        let integrity: String = db.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if integrity != "ok" {
            return Err(StoreError::Unreadable(format!(
                "database integrity: {integrity}"
            )));
        }
        let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        if !existed {
            tx.execute_batch("CREATE TABLE store_info (singleton INTEGER PRIMARY KEY CHECK(singleton=1), format INTEGER NOT NULL, project BLOB NOT NULL, scope_id TEXT NOT NULL);
                CREATE TABLE operations (operation_id TEXT PRIMARY KEY, scope_id TEXT NOT NULL, caller_id TEXT NOT NULL, request_id TEXT NOT NULL,
                    accepted TEXT NOT NULL, dispatch TEXT NOT NULL, execution TEXT NOT NULL, cancellation TEXT NOT NULL,
                    stdout TEXT NOT NULL, stderr TEXT NOT NULL, release TEXT NOT NULL, revision INTEGER NOT NULL CHECK(revision>=0),
                    UNIQUE(scope_id, caller_id, request_id));
                CREATE TABLE artifacts (operation_id TEXT NOT NULL REFERENCES operations(operation_id) ON DELETE CASCADE,
                    kind TEXT NOT NULL CHECK(kind IN ('script','stdout','stderr')), offset INTEGER NOT NULL CHECK(offset>=0),
                    digest TEXT NOT NULL, bytes INTEGER NOT NULL CHECK(bytes>=0), PRIMARY KEY(operation_id,kind,offset));")?;
            let scope = crate::hex(&Sha256::digest(dir.as_os_str().as_encoded_bytes()));
            tx.execute(
                "INSERT INTO store_info VALUES (1,?1,?2,?3)",
                params![FORMAT, project_root.as_os_str().as_encoded_bytes(), scope],
            )?;
        }
        let (format, project, scope_id): (i64, Vec<u8>, String) = tx.query_row(
            "SELECT format,project,scope_id FROM store_info WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if format != FORMAT {
            return Err(StoreError::Unreadable(format!(
                "unsupported store format {format}"
            )));
        }
        if project != project_root.as_os_str().as_encoded_bytes() {
            return Err(StoreError::Storage(
                "state dir belongs to project with a different root".into(),
            ));
        }
        tx.commit()?;
        sync_dir(&dir)?;
        mkdir(&dir.join("blobs"))?;
        mkdir(&dir.join("blobs/sha256"))?;
        mkdir(&dir.join("runs"))?;
        let store = Self {
            dir,
            scope_id,
            inner: Mutex::new(Inner {
                db,
                pins: HashSet::new(),
            }),
            _owner: owner,
        };
        match store.collect() {
            // The row is retained and reported by Core; never collect its material.
            Err(StoreError::Unreadable(_)) => {}
            result => result?,
        }
        Ok(store)
    }

    pub fn identities(&self) -> Result<Vec<(String, String)>> {
        let inner = lock(&self.inner);
        let mut stmt = inner
            .db
            .prepare("SELECT scope_id,caller_id,request_id FROM operations")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut identities = Vec::new();
        for row in rows {
            let (scope, caller, id) = row?;
            if scope != self.scope_id {
                return Err(StoreError::Unreadable(
                    "operation scope does not match this store".into(),
                ));
            }
            identities.push((caller, id));
        }
        Ok(identities)
    }

    pub fn get(&self, caller: &str, request_id: &str) -> Result<Option<Stored>> {
        let inner = lock(&self.inner);
        self.get_on(&inner.db, caller, request_id)
    }

    fn get_on(&self, db: &Connection, caller: &str, request_id: &str) -> Result<Option<Stored>> {
        let row = db.query_row("SELECT operation_id,accepted,dispatch,execution,cancellation,stdout,stderr,release,revision
            FROM operations WHERE scope_id=?1 AND caller_id=?2 AND request_id=?3",
            params![self.scope_id,caller,request_id], |r| Ok((r.get::<_,String>(0)?, r.get::<_,String>(1)?,
                r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,
                r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,u64>(8)?))).optional()?;
        let Some((id, a, d, e, c, o, s, g, revision)) = row else {
            return Ok(None);
        };
        let accepted: Accepted = parse("accepted", &a)?;
        if accepted.run_id != id
            || accepted.key.scope_id != self.scope_id
            || accepted.key.caller_id != caller
            || accepted.key.request_id != request_id
            || accepted.request.caller != caller
            || accepted.request.request_id != request_id
            || !valid_digest(&accepted.fingerprint)
            || !valid_digest(&accepted.script.sha256)
            || accepted.script.sha256 != accepted.request.code_sha256
            || accepted.script.bytes != accepted.request.code_bytes as u64
            || accepted.stream_limit == 0
            || accepted.chunk_limit == 0
            || id.contains('/')
            || id == "."
            || id == ".."
        {
            return Err(StoreError::Unreadable(
                "accepted identity, fingerprint or script metadata is inconsistent".into(),
            ));
        }
        let stdout = self.output_on(db, &id, "stdout", parse("stdout", &o)?, &accepted)?;
        let stderr = self.output_on(db, &id, "stderr", parse("stderr", &s)?, &accepted)?;
        // Empty scripts are referenced too, so collection cannot remove them
        // between acceptance and a concurrent dispatch.
        let scripts = self.blobs_on(db, &id, "script", 1)?;
        if scripts != vec![accepted.script.clone()] {
            return Err(StoreError::Unreadable(
                "script artifact does not match acceptance".into(),
            ));
        }
        let mut record = OperationRecord {
            operation_id: id,
            key: accepted.key.clone(),
            request: accepted.request.clone(),
            script: accepted.script.clone(),
            accepted_at: accepted.accepted_at,
            dispatch: parse("dispatch", &d)?,
            execution: parse("execution", &e)?,
            cancellation: parse("cancellation", &c)?,
            outputs: OutputSet { stdout, stderr },
            group_released: parse("release", &g)?,
            run_lock: Knowledge::unknown("not observed in this instance", accepted.accepted_at),
            retention: RetentionState::Protected {
                reason: "not observed".into(),
            },
            revision,
        };
        if let Some(
            ExecutionFact::SpawnObserved { pid, .. } | ExecutionFact::ExitObserved { pid, .. },
        ) = record.execution.value()
            && (*pid == 0 || record.dispatch.value() != Some(&DispatchFact::Attempted))
        {
            return Err(StoreError::Unreadable(
                "native execution without a valid dispatch/pid".into(),
            ));
        }
        record.retention(false, false);
        Ok(Some(Stored { accepted, record }))
    }

    fn blobs_on(
        &self,
        db: &Connection,
        id: &str,
        kind: &str,
        limit: usize,
    ) -> Result<Vec<BlobRef>> {
        let max = limit
            .checked_add(1)
            .ok_or_else(|| StoreError::Unreadable("artifact count limit overflow".into()))?;
        let mut stmt = db.prepare("SELECT offset,digest,bytes FROM artifacts WHERE operation_id=?1 AND kind=?2 ORDER BY offset LIMIT ?3")?;
        let rows = stmt.query_map(params![id, kind, max], |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, u64>(2)?,
            ))
        })?;
        let mut offset = 0u64;
        let mut blobs = Vec::new();
        for row in rows {
            let (at, sha256, bytes) = row?;
            if at != offset
                || (bytes == 0 && kind != "script")
                || !valid_digest(&sha256)
                || (kind != "script" && bytes > BLOB_CHUNK as u64)
            {
                return Err(StoreError::Unreadable(format!(
                    "{kind} artifact offsets, digest or length are invalid"
                )));
            }
            offset = offset
                .checked_add(bytes)
                .ok_or_else(|| StoreError::Unreadable("artifact length overflow".into()))?;
            blobs.push(BlobRef { sha256, bytes });
        }
        if blobs.len() > limit {
            return Err(StoreError::Unreadable(
                "artifact count exceeds the accepted limit".into(),
            ));
        }
        Ok(blobs)
    }

    fn output_on(
        &self,
        db: &Connection,
        id: &str,
        kind: &str,
        metadata: OutputMetadata,
        accepted: &Accepted,
    ) -> Result<OutputStream> {
        let info = metadata.info;
        let blobs = self.blobs_on(db, id, kind, accepted.chunk_limit)?;
        if info.limit_bytes != accepted.stream_limit
            || info.retained_bytes > info.limit_bytes
            || info.observed_bytes < info.retained_bytes
            || blobs.len() > accepted.chunk_limit
            || blobs.iter().map(|b| b.bytes).sum::<u64>() != info.retained_bytes
        {
            return Err(StoreError::Unreadable(format!(
                "{kind} counters do not match its artifacts/limits"
            )));
        }
        Ok(OutputStream {
            info,
            blobs,
            observed_at: metadata.observed_at,
            source: metadata.source,
        })
    }

    /// The uniqueness check, capacity check and insert are in one write transaction.
    pub fn accept(
        &self,
        mut accepted: Accepted,
        code: &str,
        max_records: usize,
    ) -> Result<Acceptance> {
        let mut inner = lock(&self.inner);
        self.collect_on(&inner)?;
        let tx = inner
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) =
            self.get_on(&tx, &accepted.key.caller_id, &accepted.key.request_id)?
        {
            return Ok(Acceptance::Existing(Box::new(existing)));
        }
        self.validate_keys(&tx)?;
        let count: usize = tx.query_row("SELECT count(*) FROM operations", [], |r| r.get(0))?;
        if count >= max_records {
            return Err(StoreError::Capacity(max_records));
        }
        accepted.script = self.write_blob(code.as_bytes())?;
        let now = accepted.accepted_at;
        let dispatch = Knowledge::Known {
            value: DispatchFact::NotAttempted,
            observed_at: now,
            source: FactSource::Core,
        };
        let execution: Knowledge<ExecutionFact> =
            Knowledge::unknown("no native start or exit observation has been saved", now);
        let release: Knowledge<bool> =
            Knowledge::unknown("native completion and release have not been observed", now);
        let stream = OutputMetadata {
            info: crate::run::empty_stream(accepted.stream_limit),
            observed_at: now,
            source: FactSource::Core,
        };
        tx.execute(
            "INSERT INTO operations VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,0)",
            params![
                accepted.run_id,
                accepted.key.scope_id,
                accepted.key.caller_id,
                accepted.key.request_id,
                json(&accepted)?,
                json(&dispatch)?,
                json(&execution)?,
                "null",
                json(&stream)?,
                json(&stream)?,
                json(&release)?
            ],
        )?;
        insert_blob(&tx, &accepted.run_id, "script", 0, &accepted.script)?;
        fault::point("staged");
        tx.commit()?;
        let stored = self
            .get_on(&inner.db, &accepted.key.caller_id, &accepted.key.request_id)?
            .ok_or_else(|| StoreError::Unreadable("committed acceptance disappeared".into()))?;
        Ok(Acceptance::New(Box::new(stored)))
    }

    pub fn blob(&self, id: &str, kind: &str, bytes: &[u8]) -> Result<BlobRef> {
        let mut inner = lock(&self.inner);
        let blob = self.write_blob(bytes)?;
        inner
            .pins
            .insert((id.into(), kind.into(), blob.sha256.clone()));
        Ok(blob)
    }

    fn write_blob(&self, bytes: &[u8]) -> Result<BlobRef> {
        let blob = BlobRef {
            sha256: crate::hex(&Sha256::digest(bytes)),
            bytes: bytes.len() as u64,
        };
        let path = self.blob_path(&blob)?;
        let parent = path.parent().expect("blob has parent");
        mkdir(parent)?;
        if path.try_exists()? {
            self.verify_blob(&blob)?;
            return Ok(blob);
        }
        let mut temp = tempfile::NamedTempFile::new_in(parent)?;
        temp.write_all(bytes)?;
        sync_file(temp.as_file())?;
        temp.persist_noclobber(&path)
            .map_err(|e| StoreError::Storage(e.to_string()))?;
        sync_dir(parent)?;
        sync_dir(&self.dir.join("blobs/sha256"))?;
        Ok(blob)
    }

    pub fn blob_path(&self, blob: &BlobRef) -> Result<PathBuf> {
        if !valid_digest(&blob.sha256) {
            return Err(StoreError::Unreadable("invalid blob digest".into()));
        }
        for relative in [
            "blobs".to_owned(),
            "blobs/sha256".to_owned(),
            format!("blobs/sha256/{}", &blob.sha256[..2]),
        ] {
            match fs::symlink_metadata(self.dir.join(relative)) {
                Ok(metadata) if !metadata.file_type().is_dir() => {
                    return Err(StoreError::Unreadable(
                        "blob directory has been replaced or linked outside the store".into(),
                    ));
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(self
            .dir
            .join("blobs/sha256")
            .join(&blob.sha256[..2])
            .join(&blob.sha256))
    }

    pub fn verify_blob(&self, blob: &BlobRef) -> Result<()> {
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.blob_path(blob)?)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() != blob.bytes {
            return Err(StoreError::Unreadable("blob length/type mismatch".into()));
        }
        let mut hash = Sha256::new();
        let mut buf = [0; BLOB_CHUNK];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        if crate::hex(&hash.finalize()) != blob.sha256 {
            return Err(StoreError::Unreadable("blob checksum mismatch".into()));
        }
        Ok(())
    }

    pub fn read_blob(&self, blob: &BlobRef) -> Result<Vec<u8>> {
        // Output blobs are bounded, and the bytes returned are the bytes hashed.
        if blob.bytes > BLOB_CHUNK as u64 {
            return Err(StoreError::Unreadable(
                "output blob exceeds chunk bound".into(),
            ));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.blob_path(blob)?)?;
        let mut bytes = Vec::new();
        (&mut file).take(blob.bytes + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 != blob.bytes || crate::hex(&Sha256::digest(&bytes)) != blob.sha256 {
            return Err(StoreError::Unreadable(
                "output blob checksum/length mismatch".into(),
            ));
        }
        Ok(bytes)
    }

    /// Saves just one dimension. No output failure can invent an exit or cancellation.
    pub fn save(&self, record: &OperationRecord, fact: Fact) -> Result<u64> {
        let mut inner = lock(&self.inner);
        let tx = inner
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = self
            .get_on(&tx, &record.key.caller_id, &record.key.request_id)?
            .ok_or_else(|| StoreError::Unreadable("accepted operation disappeared".into()))?;
        if old.record.operation_id != record.operation_id || old.record.revision != record.revision
        {
            return Err(StoreError::Storage(
                "operation revision changed; refusing stale fact update".into(),
            ));
        }
        let value = match fact {
            Fact::Dispatch => {
                monotonic(&old.record.dispatch, &record.dispatch)?;
                if old.record.dispatch.value() == Some(&DispatchFact::Attempted)
                    && record.dispatch.value() != Some(&DispatchFact::Attempted)
                {
                    return Err(StoreError::Unreadable(
                        "dispatch knowledge regression".into(),
                    ));
                }
                json(&record.dispatch)?
            }
            Fact::Execution => {
                monotonic(&old.record.execution, &record.execution)?;
                if let (Some(a), Some(b)) = (old.record.execution.value(), record.execution.value())
                {
                    if b.rank() < a.rank() || (a.rank() == 2 && a != b) {
                        return Err(StoreError::Unreadable(
                            "execution knowledge regression".into(),
                        ));
                    }
                    if let ExecutionFact::SpawnObserved { pid, spawned_at } = a {
                        match b {
                            ExecutionFact::SpawnObserved {
                                pid: next_pid,
                                spawned_at: next_at,
                            }
                            | ExecutionFact::ExitObserved {
                                pid: next_pid,
                                spawned_at: next_at,
                                ..
                            } if pid == next_pid && spawned_at == next_at => {}
                            _ => {
                                return Err(StoreError::Unreadable(
                                    "observed native identity changed or became not-started".into(),
                                ));
                            }
                        }
                    }
                }
                json(&record.execution)?
            }
            Fact::Release => {
                monotonic(&old.record.group_released, &record.group_released)?;
                if old.record.group_released.value() == Some(&true)
                    && record.group_released.value() != Some(&true)
                {
                    return Err(StoreError::Unreadable(
                        "release knowledge regression".into(),
                    ));
                }
                json(&record.group_released)?
            }
            Fact::Cancel => {
                if let Some(a) = &old.record.cancellation {
                    let b = record
                        .cancellation
                        .as_ref()
                        .ok_or_else(|| StoreError::Unreadable("cancel intent lost".into()))?;
                    if a.requested_at != b.requested_at
                        || a.reason != b.reason
                        || (a.term_sent_at.is_some() && a.term_sent_at != b.term_sent_at)
                        || (a.kill_sent_at.is_some() && a.kill_sent_at != b.kill_sent_at)
                    {
                        return Err(StoreError::Unreadable("cancel knowledge regression".into()));
                    }
                }
                json(&record.cancellation)?
            }
            Fact::Stdout | Fact::Stderr => {
                let (kind, a, b) = if fact == Fact::Stdout {
                    ("stdout", &old.record.outputs.stdout, &record.outputs.stdout)
                } else {
                    ("stderr", &old.record.outputs.stderr, &record.outputs.stderr)
                };
                if b.info.retained_bytes < a.info.retained_bytes
                    || b.info.observed_bytes < a.info.observed_bytes
                    || (a.info.eof && !b.info.eof)
                    || !b.blobs.starts_with(&a.blobs)
                    || b.blobs.len() > old.accepted.chunk_limit
                    || b.info.retained_bytes > old.accepted.stream_limit
                    || b.info.observed_bytes < b.info.retained_bytes
                    || b.blobs.iter().map(|b| b.bytes).sum::<u64>() != b.info.retained_bytes
                    || b.observed_at < a.observed_at
                {
                    return Err(StoreError::Unreadable(
                        "output knowledge regression or invalid counters".into(),
                    ));
                }
                let mut offset = a.info.retained_bytes;
                for blob in &b.blobs[a.blobs.len()..] {
                    insert_blob(&tx, &record.operation_id, kind, offset, blob)?;
                    offset += blob.bytes;
                }
                json(&OutputMetadata::from(b))?
            }
        };
        let update = format!(
            "UPDATE operations SET {}=?1,revision=revision+1 WHERE operation_id=?2 AND revision=?3",
            fact.column()
        );
        if tx.execute(
            &update,
            params![value, record.operation_id, record.revision],
        )? != 1
        {
            return Err(StoreError::Storage("stale operation revision".into()));
        }
        fault::point(match fact {
            Fact::Stdout | Fact::Stderr => "output-staged",
            _ => "fact-staged",
        });
        tx.commit()?;
        if matches!(fact, Fact::Stdout | Fact::Stderr) {
            inner
                .pins
                .retain(|(id, kind, _)| id != &record.operation_id || kind != fact.column());
        }
        Ok(record.revision + 1)
    }

    pub fn discard(&self, record: &OperationRecord) -> Result<()> {
        let mut inner = lock(&self.inner);
        let tx = inner
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let old = self
            .get_on(&tx, &record.key.caller_id, &record.key.request_id)?
            .ok_or_else(|| {
                StoreError::Unreadable("operation missing before explicit deletion".into())
            })?;
        if old.record.operation_id != record.operation_id
            || old.record.revision != record.revision
            || !old.record.removable()
        {
            return Err(StoreError::Storage(
                "operation is protected or its revision changed".into(),
            ));
        }
        tx.execute(
            "DELETE FROM operations WHERE operation_id=?1 AND revision=?2",
            params![record.operation_id, record.revision],
        )?;
        fault::point("delete-staged");
        tx.commit()?;
        fault::point("deleted");
        let _ = fs::remove_dir_all(self.run_dir(&record.operation_id));
        // Cleanup errors do not undo the explicit, committed key release.
        let _ = self.collect_on(&inner);
        Ok(())
    }

    pub fn run_dir(&self, id: &str) -> PathBuf {
        self.dir.join("runs").join(id)
    }

    fn collect(&self) -> Result<()> {
        self.collect_on(&lock(&self.inner))
    }
    fn collect_on(&self, inner: &Inner) -> Result<()> {
        // Keep all material when row identities or references are damaged.
        self.validate_keys(&inner.db)?;
        let mut stmt = inner.db.prepare("SELECT DISTINCT digest FROM artifacts")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut referenced: HashSet<String> = rows.collect::<std::result::Result<_, _>>()?;
        referenced.extend(inner.pins.iter().map(|(_, _, digest)| digest.clone()));
        for prefix in fs::read_dir(self.dir.join("blobs/sha256"))? {
            let prefix = prefix?;
            if !prefix.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(prefix.path())? {
                let file = file?;
                if file.file_type()?.is_file()
                    && !referenced.contains(&file.file_name().to_string_lossy().to_string())
                {
                    fs::remove_file(file.path())?;
                }
            }
        }
        Ok(())
    }

    fn validate_keys(&self, db: &Connection) -> Result<()> {
        let mut stmt = db.prepare("SELECT scope_id,caller_id,request_id FROM operations")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (scope, caller, id) = row?;
            if scope != self.scope_id {
                return Err(StoreError::Unreadable(
                    "operation scope does not match this store".into(),
                ));
            }
            self.get_on(db, &caller, &id)?
                .ok_or_else(|| StoreError::Unreadable("listed operation disappeared".into()))?;
        }
        Ok(())
    }
}

fn valid_digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn insert_blob(db: &Connection, id: &str, kind: &str, offset: u64, blob: &BlobRef) -> Result<()> {
    db.execute(
        "INSERT INTO artifacts VALUES (?1,?2,?3,?4,?5)",
        params![id, kind, offset, blob.sha256, blob.bytes],
    )?;
    Ok(())
}
fn monotonic<T>(old: &Knowledge<T>, new: &Knowledge<T>) -> Result<()> {
    if let Knowledge::Known { observed_at: a, .. } = old {
        match new {
            Knowledge::Known { observed_at: b, .. } if b >= a => {}
            _ => {
                return Err(StoreError::Unreadable(
                    "known observation cannot become older or unknown".into(),
                ));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Core, CoreConfig, Executor, RunRequest, SubmitError};

    fn fixture() -> (tempfile::TempDir, Core, RunRequest) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("project");
        fs::create_dir(&root).unwrap();
        let core = Core::open(
            CoreConfig::new(&root, tmp.path().join("state"))
                .executor(Executor::new("sh", "/bin/sh")),
        )
        .unwrap();
        let request = RunRequest {
            request_id: "job".into(),
            executor: "sh".into(),
            workdir: ".".into(),
            code: "echo native >> effects\nprintf output\n".into(),
            args: vec![],
        };
        (tmp, core, request)
    }

    #[test]
    fn durability_settings_use_a_patched_sqlite_wal_engine() {
        let (_tmp, core, _request) = fixture();
        // SQLite <= 3.51.2 has the WAL-reset corruption bug. The bundled
        // engine must contain its fix, independently of the Rust wrapper.
        assert!(rusqlite::version_number() >= 3_051_003);
        let inner = lock(&core.store.inner);
        let mode: String = inner
            .db
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        for (pragma, expected) in [("synchronous", 2), ("fullfsync", 1), ("foreign_keys", 1)] {
            let value: i64 = inner
                .db
                .query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(value, expected, "{pragma}");
        }
    }

    #[test]
    fn sqlite_full_rolls_back_acceptance_before_any_native_dispatch() {
        let (_tmp, core, mut request) = fixture();
        request.args.push("x".repeat(63 * 1024));
        {
            let inner = lock(&core.store.inner);
            let pages: u64 = inner
                .db
                .query_row("PRAGMA page_count", [], |r| r.get(0))
                .unwrap();
            inner
                .db
                .pragma_update(None, "max_page_count", pages)
                .unwrap();
        }
        let error = core.submit("agent", request.clone()).unwrap_err();
        assert!(matches!(error, SubmitError::Storage(_)), "{error:?}");
        assert!(error.to_string().contains("full"), "{error}");
        assert!(!core.project_root.join("effects").exists());
        assert!(matches!(
            core.lookup("agent", "job"),
            Err(crate::LookupError::NotFound { .. })
        ));
        lock(&core.store.inner)
            .db
            .pragma_update(None, "max_page_count", 1024)
            .unwrap();
        core.submit("agent", request).unwrap();
        assert!(
            core.wait("agent", "job", Duration::from_secs(10))
                .unwrap()
                .is_terminal()
        );
        assert_eq!(
            fs::read_to_string(core.project_root.join("effects")).unwrap(),
            "native\n"
        );
    }

    #[test]
    fn sqlite_read_only_refuses_acceptance_without_dispatch() {
        let (_tmp, core, request) = fixture();
        lock(&core.store.inner)
            .db
            .pragma_update(None, "query_only", true)
            .unwrap();
        let error = core.submit("agent", request.clone()).unwrap_err();
        assert!(error.to_string().contains("readonly"), "{error}");
        assert!(!core.project_root.join("effects").exists());
        lock(&core.store.inner)
            .db
            .pragma_update(None, "query_only", false)
            .unwrap();
        core.submit("agent", request).unwrap();
        assert!(
            core.wait("agent", "job", Duration::from_secs(10))
                .unwrap()
                .is_terminal()
        );
    }

    #[test]
    fn known_facts_and_output_cursors_cannot_regress_or_overwrite_a_new_revision() {
        let (_tmp, core, request) = fixture();
        core.submit("agent", request).unwrap();
        let original = core
            .wait("agent", "job", Duration::from_secs(10))
            .unwrap()
            .operation;
        assert!(original.removable());
        let mut bad = original.clone();
        bad.execution = Knowledge::unknown("old observation", SystemTime::now());
        assert!(core.store.save(&bad, Fact::Execution).is_err());
        let mut bad = original.clone();
        bad.execution = Knowledge::known(
            ExecutionFact::SpawnObserved {
                pid: 1,
                spawned_at: SystemTime::now(),
            },
            FactSource::NativeProcess,
        );
        assert!(core.store.save(&bad, Fact::Execution).is_err());
        let mut bad = original.clone();
        bad.outputs.stdout.info.retained_bytes = 0;
        bad.outputs.stdout.blobs.clear();
        assert!(core.store.save(&bad, Fact::Stdout).is_err());
        let revision = core.store.save(&original, Fact::Stdout).unwrap();
        assert!(revision > original.revision);
        assert!(core.store.save(&original, Fact::Release).is_err());
        let saved = core.store.get("agent", "job").unwrap().unwrap().record;
        assert_eq!(saved.execution, original.execution);
        assert_eq!(saved.outputs, original.outputs);
    }
}
