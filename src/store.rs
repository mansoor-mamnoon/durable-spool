//! Bounded atomic snapshots protected by a stable, never-renamed lock inode.
use std::fs::{self, DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

pub const MAX_JOBS: usize = 10_000;
const MAX_STATE: usize = 64 * 1024 * 1024;
const MAX_FIELD: usize = 1024 * 1024;
const MAGIC: &[u8; 8] = b"DSPOOL01";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Pending,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl Status {
    /// Return the stable human-readable status name.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone)]
pub struct Job {
    pub id: u64,
    pub argv: Vec<Vec<u8>>,
    pub cwd: Vec<u8>,
    pub status: Status,
    pub attempts: u64,
    pub max_attempts: u64,
    pub backoff_ms: u64,
    pub ready_at: u64,
    pub result: Vec<u8>,
}

pub struct State {
    pub next_id: u64,
    pub jobs: Vec<Job>,
}

impl State {
    /// Locate a job without accepting path-like identifiers.
    pub fn job_mut(&mut self, id: u64) -> io::Result<&mut Job> {
        self.jobs
            .iter_mut()
            .find(|job| job.id == id)
            .ok_or_else(|| io::Error::other("job not found"))
    }
}

pub struct Store {
    pub root: PathBuf,
}

fn private_dir(path: &Path) -> io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("queue has no parent"))?;
    private_dir(parent)?;
    match DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() => (),
        Err(error) => return Err(error),
    }
    File::open(parent)?.sync_all()
}

/// Open a private file without truncating an existing lock or log.
pub fn open_file(path: &Path) -> io::Result<File> {
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
}

impl Store {
    /// Create the queue directory tree and persist newly created directory entries.
    pub fn open(path: &Path) -> io::Result<Self> {
        let absolute = std::env::current_dir()?.join(path);
        private_dir(&absolute)?;
        let root = absolute.canonicalize()?;
        // Sync even when a competing creator made the directory before this call.
        for ancestor in root.ancestors().skip(1) {
            File::open(ancestor)?.sync_all()?;
        }
        private_dir(&root.join("logs"))?;
        Ok(Self { root })
    }

    fn state_lock(&self) -> io::Result<File> {
        let file = open_file(&self.root.join("state.lock"))?;
        file.lock()?;
        Ok(file)
    }

    /// Acquire the lifetime worker lock, failing immediately on contention.
    pub fn worker_lock(&self) -> io::Result<File> {
        let file = open_file(&self.root.join("worker.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(TryLockError::WouldBlock) => {
                Err(io::Error::other("another worker holds this queue"))
            }
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Read one committed snapshot under the transaction lock.
    pub fn read(&self) -> io::Result<State> {
        let _lock = self.state_lock()?;
        self.load()
    }

    /// Serialize a mutation and acknowledge it only after file and directory sync.
    pub fn update<T>(&self, change: impl FnOnce(&mut State) -> io::Result<T>) -> io::Result<T> {
        let _lock = self.state_lock()?;
        let mut state = self.load()?;
        let result = change(&mut state)?;
        let bytes = encode(&state)?;
        let temporary = self.root.join("state.new");
        let mut file = open_file(&temporary)?;
        file.set_len(0)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, self.root.join("state"))?;
        File::open(&self.root)?.sync_all()?;
        Ok(result)
    }

    fn load(&self) -> io::Result<State> {
        let file = match File::open(self.root.join("state")) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(State {
                    next_id: 1,
                    jobs: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take((MAX_STATE + 1) as u64).read_to_end(&mut bytes)?;
        decode(&bytes)
    }
}

fn corrupt() -> io::Error {
    io::Error::other("corrupt or unsupported queue snapshot; refusing to reset")
}

fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

fn number(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn field(bytes: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
    if value.len() > MAX_FIELD {
        return Err(io::Error::other("field exceeds 1 MiB"));
    }
    number(bytes, value.len() as u64);
    bytes.extend_from_slice(value);
    Ok(())
}

fn encode(state: &State) -> io::Result<Vec<u8>> {
    let mut bytes = MAGIC.to_vec();
    number(&mut bytes, state.next_id);
    number(&mut bytes, state.jobs.len() as u64);
    for job in &state.jobs {
        number(&mut bytes, job.id);
        bytes.push(job.status as u8);
        for value in [job.attempts, job.max_attempts, job.backoff_ms, job.ready_at] {
            number(&mut bytes, value);
        }
        field(&mut bytes, &job.cwd)?;
        field(&mut bytes, &job.result)?;
        number(&mut bytes, job.argv.len() as u64);
        for arg in &job.argv {
            field(&mut bytes, arg)?;
        }
        if bytes.len() > MAX_STATE - 8 {
            return Err(io::Error::other("queue exceeds 64 MiB; use a new queue"));
        }
    }
    let hash = checksum(&bytes);
    number(&mut bytes, hash);
    Ok(bytes)
}

struct Decoder<'a> {
    bytes: &'a [u8],
}

impl<'a> Decoder<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        if count > self.bytes.len() {
            return Err(corrupt());
        }
        let (value, rest) = self.bytes.split_at(count);
        self.bytes = rest;
        Ok(value)
    }

    fn number(&mut self) -> io::Result<u64> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().map_err(|_| corrupt())?,
        ))
    }

    fn field(&mut self) -> io::Result<Vec<u8>> {
        let count = self.number()?;
        if count > MAX_FIELD as u64 {
            return Err(corrupt());
        }
        Ok(self.take(count as usize)?.to_vec())
    }
}

fn decode(bytes: &[u8]) -> io::Result<State> {
    if bytes.len() < 32 || bytes.len() > MAX_STATE {
        return Err(corrupt());
    }
    let (payload, hash) = bytes.split_at(bytes.len() - 8);
    if checksum(payload) != u64::from_le_bytes(hash.try_into().map_err(|_| corrupt())?) {
        return Err(corrupt());
    }
    let mut decoder = Decoder { bytes: payload };
    if decoder.take(8)? != MAGIC {
        return Err(corrupt());
    }
    let next_id = decoder.number()?;
    let count = decoder.number()?;
    if count > MAX_JOBS as u64 || next_id != count + 1 {
        return Err(corrupt());
    }
    let mut jobs = Vec::new();
    for expected_id in 1..=count {
        let id = decoder.number()?;
        let status = match decoder.take(1)?[0] {
            0 => Status::Pending,
            1 => Status::Running,
            2 => Status::Succeeded,
            3 => Status::Failed,
            4 => Status::Cancelled,
            _ => return Err(corrupt()),
        };
        let attempts = decoder.number()?;
        let max_attempts = decoder.number()?;
        let backoff_ms = decoder.number()?;
        let ready_at = decoder.number()?;
        let cwd = decoder.field()?;
        let result = decoder.field()?;
        let argc = decoder.number()?;
        if id != expected_id
            || !(1..=100).contains(&max_attempts)
            || attempts > max_attempts
            || backoff_ms > 60_000
            || !(1..=65_536).contains(&argc)
            || cwd.first() != Some(&b'/')
            || cwd.contains(&0)
        {
            return Err(corrupt());
        }
        let mut argv = Vec::new();
        for _ in 0..argc {
            let arg = decoder.field()?;
            if arg.contains(&0) {
                return Err(corrupt());
            }
            argv.push(arg);
        }
        if argv[0].is_empty() {
            return Err(corrupt());
        }
        jobs.push(Job {
            id,
            argv,
            cwd,
            status,
            attempts,
            max_attempts,
            backoff_ms,
            ready_at,
            result,
        });
    }
    if !decoder.bytes.is_empty() {
        return Err(corrupt());
    }
    Ok(State { next_id, jobs })
}
