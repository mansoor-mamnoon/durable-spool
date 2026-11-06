//! Claim before spawn; acknowledge only after wait and log synchronization.
use crate::store::{Job, Status, Store, open_file};
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Return a wall-clock timestamp used only for retry eligibility, never identity.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

fn retry(job: &mut Job, message: String) {
    job.result = message.into_bytes();
    if job.attempts < job.max_attempts {
        job.status = Status::Pending;
        let delay = job
            .backoff_ms
            .saturating_mul(1u64 << job.attempts.saturating_sub(1).min(16))
            .min(60_000);
        job.ready_at = now_ms().saturating_add(delay);
    } else {
        job.status = Status::Failed;
    }
}

/// Explicitly authorize replay of interrupted attempts after checking for orphan children.
pub fn recover(store: &Store) -> io::Result<usize> {
    let _worker = store.worker_lock()?;
    store.update(|state| {
        let mut count = 0;
        for job in &mut state.jobs {
            if job.status == Status::Running {
                retry(
                    job,
                    "interrupted: outcome unknown; recovery authorized".into(),
                );
                count += 1;
            }
        }
        Ok(count)
    })
}

fn execute(store: &Store, job: &Job) -> io::Result<(bool, String)> {
    let logs = store.root.join("logs");
    let stdout = open_file(&logs.join(format!("{}.{}.stdout", job.id, job.attempts)))?;
    let stderr = open_file(&logs.join(format!("{}.{}.stderr", job.id, job.attempts)))?;
    // Attempts never reuse log names. Keep parent handles to sync after wait.
    File::open(store.root.join("logs"))?.sync_all()?;
    let argv: Vec<_> = job.argv.iter().cloned().map(OsString::from_vec).collect();
    let result = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(OsString::from_vec(job.cwd.clone()))
        .env("DURABLE_SPOOL_JOB_ID", job.id.to_string())
        .env("DURABLE_SPOOL_ATTEMPT", job.attempts.to_string())
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .spawn();
    // A wait error is ambiguous, not a known spawn failure. Leave Running intact.
    let result = match result {
        Ok(mut child) => Ok(child.wait()?),
        Err(error) => Err(error),
    };
    stdout.sync_all()?;
    stderr.sync_all()?;
    match result {
        Ok(status) => Ok((
            status.success(),
            match status.code() {
                Some(code) => format!("exit {code}"),
                None => "terminated by signal".into(),
            },
        )),
        Err(error) => Ok((false, format!("spawn: {error}"))),
    }
}

/// Drain eligible work; --once performs at most one attempt without waiting for backoff.
pub fn run(store: &Store, once: bool) -> io::Result<()> {
    let _worker = store.worker_lock()?;
    if store
        .read()?
        .jobs
        .iter()
        .any(|job| job.status == Status::Running)
    {
        return Err(io::Error::other(
            "interrupted job: check orphan processes, then use recover",
        ));
    }
    loop {
        let snapshot = store.read()?;
        let next = snapshot
            .jobs
            .iter()
            .filter(|job| job.status == Status::Pending)
            .map(|job| job.ready_at)
            .min();
        let Some(next) = next else {
            return Ok(());
        };
        let now = now_ms();
        if next > now {
            if once {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis((next - now).min(250)));
            continue;
        }
        let job = store.update(|state| {
            let job = state
                .jobs
                .iter_mut()
                .find(|job| job.status == Status::Pending && job.ready_at <= now_ms());
            let Some(job) = job else {
                return Ok(None);
            };
            job.status = Status::Running;
            job.attempts += 1;
            Ok(Some(job.clone()))
        })?;
        let Some(job) = job else {
            continue;
        };
        let (success, result) = execute(store, &job)?;
        store.update(|state| {
            let stored = state.job_mut(job.id)?;
            if success {
                stored.status = Status::Succeeded;
                stored.result = result.into_bytes();
            } else {
                retry(stored, result);
            }
            Ok(())
        })?;
        if once {
            return Ok(());
        }
    }
}
