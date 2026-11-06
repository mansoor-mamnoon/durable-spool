//! A local, dependency-free command queue for trusted Unix users.
#![forbid(unsafe_code)]
#[cfg(not(unix))]
compile_error!("durable-spool currently supports Unix only");
mod runner;
mod store;

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use store::{Job, MAX_JOBS, Status, Store};

const HELP: &str = "durable-spool [--queue DIR] COMMAND
  add [--attempts N] [--backoff-ms N] -- PROGRAM [ARG...]
  run [--once]    Drain queue; once performs at most one ready attempt
  list           List status and retry budget
  show ID        Show argv, cwd, result, and log directory
  cancel ID      Cancel pending work only
  recover        Authorize replay of interrupted attempts (check orphans first)
  help

Default queue: .spool in the current directory
Defaults: 3 attempts, 1000 ms backoff; capped at 60 seconds.
No shell expansion. Use an explicit shell only for trusted shell scripts.
At-least-once attempts, NOT exactly-once effects. See DESIGN.md.";

fn usage() -> io::Error { io::Error::other(format!("usage: {HELP}")) }

fn integer(value: &OsStr, name: &str) -> io::Result<u64> {
    value.to_str().and_then(|value| value.parse().ok())
        .ok_or_else(|| io::Error::other(format!("invalid {name}")))
}

fn add(store: &Store, args: &[OsString]) -> io::Result<u64> {
    let mut max_attempts = 3;
    let mut backoff_ms = 1000;
    let mut offset = 0;
    while args.get(offset).is_some_and(|arg| arg != "--") {
        let value = args.get(offset + 1).ok_or_else(usage)?;
        match args[offset].to_str() {
            Some("--attempts") => max_attempts = integer(value, "attempts")?,
            Some("--backoff-ms") => backoff_ms = integer(value, "backoff")?,
            _ => return Err(io::Error::other("unknown add option; use -- before command")),
        }
        offset += 2;
    }
    if !(1..=100).contains(&max_attempts) { return Err(io::Error::other("attempts must be 1..100")); }
    if backoff_ms > 60_000 { return Err(io::Error::other("backoff must be 0..60000 ms")); }
    let argv: Vec<Vec<u8>> = args.get(offset + 1..).unwrap_or_default()
        .iter().map(|arg| arg.as_bytes().to_vec()).collect();
    if argv.is_empty() || argv[0].is_empty() || argv.len() > 65_536 {
        return Err(io::Error::other("provide a nonempty command after -- (at most 65536 arguments)"));
    }
    let cwd = std::env::current_dir()?.as_os_str().as_bytes().to_vec();
    store.update(|state| {
        if state.jobs.len() >= MAX_JOBS { return Err(io::Error::other("queue has 10000 jobs; use a new queue")); }
        let id = state.next_id;
        state.next_id += 1;
        state.jobs.push(Job { id, argv, cwd, status: Status::Pending, attempts: 0,
            max_attempts, backoff_ms, ready_at: 0, result: Vec::new() });
        Ok(id)
    })
}

fn row(job: &Job) {
    println!("{}\t{}\t{}/{}\t{}", job.id, job.status.name(), job.attempts,
        job.max_attempts, job.ready_at);
}

fn cli() -> io::Result<()> {
    let mut args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut queue = PathBuf::from(".spool");
    if args.first().is_some_and(|arg| arg == "--queue") {
        if args.len() < 3 { return Err(usage()); }
        queue = PathBuf::from(&args[1]);
        args.drain(..2);
    }
    let command = args.first().and_then(|arg| arg.to_str()).ok_or_else(usage)?;
    if (command == "help" || command == "--help") && args.len() == 1 {
        println!("{HELP}");
        return Ok(());
    }
    let store = Store::open(&queue)?;
    match (command, &args[1..]) {
        ("add", rest) => println!("{}", add(&store, rest)?),
        ("run", []) => runner::run(&store, false)?,
        ("run", [once]) if once == "--once" => runner::run(&store, true)?,
        ("recover", []) => println!("recovered {}", runner::recover(&store)?),
        ("list", []) => {
            println!("ID\tSTATUS\tATTEMPTS\tREADY_AT_MS");
            for job in &store.read()?.jobs { row(job); }
        }
        ("show", [id]) => {
            let id = integer(id, "id")?;
            let mut state = store.read()?;
            let job = state.job_mut(id)?;
            row(job);
            println!("cwd: {:?}", OsStr::from_bytes(&job.cwd));
            for (index, arg) in job.argv.iter().enumerate() {
                println!("argv[{index}]: {:?}", OsStr::from_bytes(arg));
            }
            println!("result: {}", String::from_utf8_lossy(&job.result));
            println!("logs: {:?}", store.root.join("logs"));
        }
        ("cancel", [id]) => {
            let id = integer(id, "id")?;
            store.update(|state| {
                let job = state.job_mut(id)?;
                if job.status != Status::Pending { return Err(io::Error::other("only pending jobs can be cancelled")); }
                job.status = Status::Cancelled;
                Ok(())
            })?;
        }
        _ => return Err(usage()),
    }
    Ok(())
}

fn main() {
    if let Err(error) = cli() {
        eprintln!("durable-spool: {error}");
        std::process::exit(1);
    }
}
