//! Execute the benchmark as a real process against a real spool executable.
//! Defensive I/O errors (disk full, non-UTF-8 child output, corrupt success rows)
//! are not injected: reproducing those would require doubles or racy tampering.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Harness {
    root: PathBuf,
    binary: PathBuf,
}

impl Harness {
    fn new(name: &str) -> Self {
        let base = std::env::var_os("DURABLE_SPOOL_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/e2e"));
        fs::create_dir_all(&base).unwrap();
        let root = base.join(format!("benchmark-{name}-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let binary = root.join("benchmark");
        let compile = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "-o"])
            .arg(&binary)
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/benchmark.rs"))
            .output()
            .unwrap();
        assert!(compile.status.success(), "{compile:?}");
        Self { root, binary }
    }

    fn run(&self, binary: &str, destination: &str, sizes: &str, trials: &str) -> Output {
        Command::new(&self.binary)
            .arg(binary)
            .arg(self.root.join(destination))
            .args([sizes, trials])
            .output()
            .unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn benchmark_records_real_trials_and_retained_successes() {
    let harness = Harness::new("success");
    let output = harness.run(env!("CARGO_BIN_EXE_durable-spool"), "run", "1,3", "2");
    assert!(output.status.success(), "{output:?}");
    let raw = fs::read_to_string(harness.root.join("run/raw.csv")).unwrap();
    let summary = fs::read_to_string(harness.root.join("run/summary.csv")).unwrap();
    assert_eq!(raw.lines().count(), 21);
    assert_eq!(summary.lines().count(), 5);
    for row in summary.lines().skip(1) {
        let fields: Vec<_> = row.split(',').collect();
        assert_eq!(fields.len(), 11);
        let size: usize = fields[0].parse().unwrap();
        let trial = fields[1];
        assert_eq!(fields[10].parse::<usize>().unwrap(), size);
        let mut add = Vec::new();
        let mut spawn = Vec::new();
        let mut drain = Vec::new();
        for sample in raw.lines().skip(1) {
            let values: Vec<_> = sample.split(',').collect();
            if values[0] == fields[0] && values[1] == trial {
                let elapsed = values[4].parse::<u128>().unwrap();
                match values[2] {
                    "add" => add.push(elapsed),
                    "spawn" => spawn.push(elapsed),
                    "drain" => drain.push(elapsed),
                    _ => panic!("unexpected operation"),
                }
            }
        }
        assert_eq!(add.len(), size);
        assert_eq!(spawn.len(), size);
        assert_eq!(drain, vec![fields[6].parse::<u128>().unwrap()]);
        add.sort_unstable();
        spawn.sort_unstable();
        assert_eq!(
            fields[2].parse::<u128>().unwrap(),
            add[(size * 50).div_ceil(100) - 1]
        );
        assert_eq!(
            fields[3].parse::<u128>().unwrap(),
            add[(size * 95).div_ceil(100) - 1]
        );
        assert_eq!(
            fields[8].parse::<u128>().unwrap(),
            spawn[(size * 50).div_ceil(100) - 1]
        );
        assert_eq!(
            fields[9].parse::<u128>().unwrap(),
            spawn[(size * 95).div_ceil(100) - 1]
        );
        for (duration, rate) in [(4, 5), (6, 7)] {
            let expected = size as f64 * 1e9 / fields[duration].parse::<f64>().unwrap();
            assert_eq!(fields[rate], format!("{expected:.6}"));
        }
        let queue = harness.root.join(format!("run/queue-{size}-{trial}"));
        let listed = Command::new(env!("CARGO_BIN_EXE_durable-spool"))
            .arg("--queue")
            .arg(&queue)
            .arg("list")
            .output()
            .unwrap();
        assert!(listed.status.success());
        let listed = String::from_utf8(listed.stdout).unwrap();
        assert_eq!(listed.lines().count(), size + 1);
        for id in 1..=size {
            assert!(listed.contains(&format!("{id}\tsucceeded\t1/1\t0\n")));
            for stream in ["stdout", "stderr"] {
                assert_eq!(
                    fs::metadata(queue.join(format!("logs/{id}.1.{stream}")))
                        .unwrap()
                        .len(),
                    0
                );
            }
        }
    }
}

#[test]
fn benchmark_rejects_invalid_inputs_and_failed_commands() {
    let harness = Harness::new("errors");
    let binary = env!("CARGO_BIN_EXE_durable-spool");
    let output = Command::new(&harness.binary).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage:"));
    for (index, sizes, trials, expected) in [
        (0, "0", "1", "sizes must"),
        (1, "10001", "1", "sizes must"),
        (2, "1", "0", "trials positive"),
        (3, "1,1", "1", "distinct"),
        (4, "no", "1", "invalid digit"),
        (5, "1", "no", "invalid digit"),
    ] {
        let output = harness.run(binary, &format!("bad-{index}"), sizes, trials);
        assert!(!output.status.success(), "{output:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "{output:?}"
        );
    }
    let missing = harness.run("/no-such-benchmark-executable", "missing", "1", "1");
    assert!(!missing.status.success());
    let existing = harness.run(binary, ".", "1", "1");
    assert!(!existing.status.success());
    let failed = harness.run("/usr/bin/false", "failed", "1", "1");
    assert!(!failed.status.success());
    let no_ids = harness.run("/usr/bin/true", "no-ids", "1", "1");
    assert!(!no_ids.status.success());
    assert!(String::from_utf8_lossy(&no_ids.stderr).contains("unexpected submitted job ID"));
}
