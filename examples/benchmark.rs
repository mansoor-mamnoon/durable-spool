//! End-to-end, dependency-free benchmark; see BENCHMARKS.md for interpretation.
#![forbid(unsafe_code)]
use std::env;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Instant;

fn checked(command: &mut Command) -> io::Result<Output> {
    let output = command.output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "{command:?}: {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

fn cli(binary: &Path, queue: &Path) -> Command {
    let mut command = Command::new(binary);
    command.arg("--queue").arg(queue);
    command
}

fn percentile(values: &[u128], percent: usize) -> u128 {
    values[(values.len() * percent).div_ceil(100) - 1]
}

fn validate(binary: &Path, queue: &Path, size: usize) -> io::Result<()> {
    let output = checked(cli(binary, queue).arg("list"))?;
    let text = String::from_utf8(output.stdout).map_err(io::Error::other)?;
    let rows: Vec<_> = text.lines().skip(1).collect();
    if rows.len() != size {
        return Err(io::Error::other("unexpected retained job count"));
    }
    for (index, row) in rows.iter().enumerate() {
        let expected = format!("{}\tsucceeded\t1/1\t0", index + 1);
        if *row != expected {
            return Err(io::Error::other(format!("unexpected result: {row}")));
        }
    }
    Ok(())
}

fn trial(
    binary: &Path,
    root: &Path,
    size: usize,
    trial: usize,
    raw: &mut File,
    summary: &mut File,
) -> io::Result<()> {
    let queue = root.join(format!("queue-{size}-{trial}"));
    // Store samples in memory; CSV formatting and writes are outside timed regions.
    let mut spawn = Vec::with_capacity(size);
    for _ in 0..size {
        let start = Instant::now();
        checked(&mut Command::new("/usr/bin/true"))?;
        spawn.push(start.elapsed().as_nanos());
    }
    let mut add = Vec::with_capacity(size);
    let start = Instant::now();
    for id in 1..=size {
        let operation = Instant::now();
        let output =
            checked(cli(binary, &queue).args(["add", "--attempts", "1", "--", "/usr/bin/true"]))?;
        add.push(operation.elapsed().as_nanos());
        if output.stdout != format!("{id}\n").as_bytes() {
            return Err(io::Error::other("unexpected submitted job ID"));
        }
    }
    let add_ns = start.elapsed().as_nanos();
    let start = Instant::now();
    checked(cli(binary, &queue).arg("run"))?;
    let drain_ns = start.elapsed().as_nanos();
    validate(binary, &queue, size)?;
    for (operation, samples) in [("spawn", &spawn), ("add", &add)] {
        for (index, elapsed) in samples.iter().enumerate() {
            writeln!(raw, "{size},{trial},{operation},{},{elapsed}", index + 1)?;
        }
    }
    writeln!(raw, "{size},{trial},drain,0,{drain_ns}")?;
    add.sort_unstable();
    spawn.sort_unstable();
    writeln!(
        summary,
        "{size},{trial},{},{},{add_ns},{:.6},{drain_ns},{:.6},{},{},{size}",
        percentile(&add, 50),
        percentile(&add, 95),
        size as f64 * 1e9 / add_ns as f64,
        size as f64 * 1e9 / drain_ns as f64,
        percentile(&spawn, 50),
        percentile(&spawn, 95),
    )?;
    raw.flush()?;
    summary.flush()?;
    eprintln!("validated size={size} trial={trial}: {size} succeeded");
    Ok(())
}

fn run() -> io::Result<()> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 4 {
        return Err(io::Error::other(
            "usage: benchmark RELEASE_BINARY NEW_OUTPUT_DIR SIZES_CSV TRIALS",
        ));
    }
    let binary = fs::canonicalize(&args[0])?;
    let sizes: Vec<usize> = args[2]
        .to_str()
        .ok_or_else(|| io::Error::other("sizes must be UTF-8"))?
        .split(',')
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(io::Error::other)?;
    let trials: usize = args[3]
        .to_str()
        .ok_or_else(|| io::Error::other("trials must be UTF-8"))?
        .parse()
        .map_err(io::Error::other)?;
    if trials == 0 || sizes.iter().any(|size| !(1..=10_000).contains(size)) {
        return Err(io::Error::other(
            "sizes must be 1..10000 and trials positive",
        ));
    }
    let mut unique = sizes.clone();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != sizes.len() {
        return Err(io::Error::other("sizes must be distinct"));
    }
    let root = PathBuf::from(&args[1]);
    // Refuse existing directories so repeated runs cannot reuse stale queue state.
    fs::create_dir(&root)?;
    let mut raw = File::create(root.join("raw.csv"))?;
    let mut summary = File::create(root.join("summary.csv"))?;
    writeln!(raw, "size,trial,operation,index,elapsed_ns")?;
    writeln!(
        summary,
        "size,trial,add_median_ns,add_p95_ns,add_total_ns,add_jobs_per_second,drain_ns,drain_jobs_per_second,spawn_median_ns,spawn_p95_ns,succeeded"
    )?;
    // One unmeasured CLI start and payload start; no pretend cold-cache claims.
    checked(Command::new(&binary).arg("help"))?;
    checked(&mut Command::new("/usr/bin/true"))?;
    for trial_index in 1..=trials {
        for size in &sizes {
            trial(&binary, &root, *size, trial_index, &mut raw, &mut summary)?;
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("benchmark: {error}");
        std::process::exit(1);
    }
}
