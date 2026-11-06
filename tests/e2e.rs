#![cfg(unix)]
#[cfg(target_os = "linux")]
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Queue {
    root: PathBuf,
}

impl Queue {
    fn new() -> Self {
        let base = std::env::var_os("DURABLE_SPOOL_TEST_TMP")
            .map(PathBuf::from)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/e2e"));
        let root = base.join(format!(
            "e2e-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_durable-spool"));
        command
            .arg("--queue")
            .arg(self.root.join("queue"))
            .current_dir(&self.root);
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.output(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap()
    }

    fn fail(&self, args: &[&str], needle: &str) {
        let output = self.output(args);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(needle),
            "{:?}",
            output
        );
    }

    fn worker(&self, args: &[&str]) -> Child {
        self.command()
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }
}

impl Drop for Queue {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn executes_exact_arguments_and_cwd_with_logs() {
    let q = Queue::new();
    let strange = OsString::from_vec(vec![b'x', 0xff, b' ', b'y']);
    let output = q
        .command()
        .args(["add", "--", "/usr/bin/printf", "%s\\n"])
        .arg("")
        .arg("a b")
        .arg("$(touch injected)")
        .arg("--flag")
        .arg(&strange)
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "1");
    q.ok(&["run"]);
    let mut expected = b"\na b\n$(touch injected)\n--flag\n".to_vec();
    expected.extend_from_slice(strange.as_bytes());
    expected.push(b'\n');
    assert_eq!(
        fs::read(q.root.join("queue/logs/1.1.stdout")).unwrap(),
        expected
    );
    assert!(!q.root.join("injected").exists());
    assert!(q.ok(&["show", "1"]).contains("succeeded"));
    q.ok(&["add", "--", "/bin/pwd"]);
    q.ok(&["run", "--once"]);
    let cwd = fs::read(q.root.join("queue/logs/2.1.stdout")).unwrap();
    assert_eq!(
        cwd,
        [q.root.canonicalize().unwrap().as_os_str().as_bytes(), b"\n"].concat()
    );
}

#[test]
fn retries_are_bounded_and_spawn_failure_is_recorded() {
    let q = Queue::new();
    q.ok(&[
        "add",
        "--attempts",
        "3",
        "--backoff-ms",
        "1",
        "--",
        "/bin/sh",
        "-c",
        "echo no >&2; exit 7",
    ]);
    q.ok(&["run"]);
    let show = q.ok(&["show", "1"]);
    assert!(
        show.contains("failed") && show.contains("3/3") && show.contains("exit 7"),
        "{show}"
    );
    assert_eq!(
        fs::read(q.root.join("queue/logs/1.3.stderr")).unwrap(),
        b"no\n"
    );
    q.ok(&["add", "--attempts", "1", "--", "/no/such/program"]);
    q.ok(&["run"]);
    assert!(q.ok(&["show", "2"]).contains("spawn:"));
}

#[test]
fn cancellation_and_cli_errors_do_not_execute_work() {
    let q = Queue::new();
    q.ok(&["add", "--", "/bin/sh", "-c", "touch bad"]);
    q.ok(&["cancel", "1"]);
    q.ok(&["run"]);
    assert!(!q.root.join("bad").exists());
    assert!(q.ok(&["list"]).contains("cancelled"));
    q.fail(&["cancel", "1"], "pending");
    q.fail(&["show", "99"], "not found");
    q.fail(&["cancel", "99"], "not found");
    q.fail(&["add", "--"], "command");
    q.fail(&["add", "--attempts", "0", "--", "true"], "attempts");
    q.fail(
        &["add", "--backoff-ms", "99999999", "--", "true"],
        "backoff",
    );
    q.fail(&["add", "--wat", "--", "true"], "option");
    q.fail(&["run", "bad"], "usage");
    q.fail(&["nonsense"], "usage");
    q.fail(&["show", "../state"], "id");
    q.ok(&["recover"]);
    q.ok(&["help"]);
}

#[test]
fn concurrent_producers_have_unique_ids_and_no_lost_updates() {
    let q = Queue::new();
    let mut producers: Vec<_> = (0..32)
        .map(|_| {
            q.command()
                .args(["add", "--", "/usr/bin/true"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut ids = Vec::new();
    for producer in producers.drain(..) {
        let output = producer.wait_with_output().unwrap();
        assert!(output.status.success());
        ids.push(
            String::from_utf8(output.stdout)
                .unwrap()
                .trim()
                .parse::<u64>()
                .unwrap(),
        );
    }
    ids.sort();
    assert_eq!(ids, (1..=32).collect::<Vec<_>>());
    q.ok(&["run"]);
    assert_eq!(q.ok(&["list"]).matches("succeeded").count(), 32);
}

#[test]
fn killed_worker_requires_explicit_recovery_and_preserves_attempt_budget() {
    let q = Queue::new();
    // The child writes its PID before blocking. Kill that exact child as well as
    // the worker, so the test does not leave an orphan or rely on PID guessing.
    q.ok(&["add", "--attempts", "2", "--backoff-ms", "0", "--", "/bin/sh", "-c",
        "if test ! -e started; then echo $$ > child.pid; touch started; exec /bin/sleep 60; fi; echo recovered"]);
    let mut worker = q.worker(&["run"]);
    wait_for(&q.root.join("started"));
    q.fail(&["run"], "worker");
    q.fail(&["recover"], "worker");
    q.fail(&["cancel", "1"], "pending");
    worker.kill().unwrap();
    worker.wait().unwrap();
    let pid = fs::read_to_string(q.root.join("child.pid")).unwrap();
    assert!(
        Command::new("/bin/kill")
            .args(["-KILL", pid.trim()])
            .status()
            .unwrap()
            .success()
    );
    q.fail(&["run"], "recover");
    q.ok(&["recover"]);
    q.ok(&["run"]);
    let show = q.ok(&["show", "1"]);
    assert!(show.contains("succeeded") && show.contains("2/2"), "{show}");
    assert_eq!(
        fs::read(q.root.join("queue/logs/1.2.stdout")).unwrap(),
        b"recovered\n"
    );
}

#[test]
fn corrupted_snapshot_is_never_silently_reset() {
    let q = Queue::new();
    q.ok(&["add", "--", "/usr/bin/true"]);
    let path = q.root.join("queue/state");
    let original = fs::read(&path).unwrap();
    let mut bad = original.clone();
    let last = bad.len() - 1;
    bad[last] ^= 1;
    fs::write(&path, &bad).unwrap();
    q.fail(&["list"], "corrupt");
    assert_eq!(fs::read(&path).unwrap(), bad);
    fs::write(&path, &original[..10]).unwrap();
    q.fail(&["run"], "corrupt");
}

#[test]
fn leftover_temporary_snapshot_is_not_committed() {
    let q = Queue::new();
    q.ok(&["add", "--", "/usr/bin/true"]);
    fs::write(q.root.join("queue/state.new"), b"interrupted write").unwrap();
    assert!(q.ok(&["list"]).contains("pending"));
    q.ok(&["add", "--", "/usr/bin/true"]);
    q.ok(&["run"]);
    assert_eq!(q.ok(&["list"]).matches("succeeded").count(), 2);
}

#[test]
fn retry_backoff_once_and_eventual_success() {
    let q = Queue::new();
    q.ok(&[
        "add",
        "--attempts",
        "3",
        "--backoff-ms",
        "400",
        "--",
        "/bin/sh",
        "-c",
        "echo $DURABLE_SPOOL_JOB_ID:$DURABLE_SPOOL_ATTEMPT; test $DURABLE_SPOOL_ATTEMPT -ge 2",
    ]);
    q.ok(&["run", "--once"]);
    assert!(q.ok(&["show", "1"]).contains("pending"));
    q.ok(&["run", "--once"]);
    assert!(!q.root.join("queue/logs/1.2.stdout").exists());
    q.ok(&["run"]);
    assert_eq!(
        fs::read(q.root.join("queue/logs/1.2.stdout")).unwrap(),
        b"1:2\n"
    );
    assert!(q.ok(&["show", "1"]).contains("succeeded"));
}

#[test]
fn producers_and_cancellation_work_while_worker_is_busy() {
    let q = Queue::new();
    q.ok(&[
        "add",
        "--",
        "/bin/sh",
        "-c",
        "touch started; while test ! -e release; do sleep 0.02; done",
    ]);
    let mut worker = q.worker(&["run"]);
    wait_for(&q.root.join("started"));
    q.ok(&["add", "--", "/bin/sh", "-c", "touch should-not-run"]);
    q.ok(&["cancel", "2"]);
    assert!(q.ok(&["show", "1"]).contains("running"));
    fs::write(q.root.join("release"), b"").unwrap();
    assert!(worker.wait().unwrap().success());
    assert!(!q.root.join("should-not-run").exists());
}

#[test]
fn recovery_exhaustion_and_signal_failure() {
    let q = Queue::new();
    // Fail log creation using a real directory, after the claim was committed.
    q.ok(&["add", "--attempts", "1", "--", "/usr/bin/true"]);
    fs::create_dir(q.root.join("queue/logs/1.1.stdout")).unwrap();
    q.fail(&["run"], "directory");
    assert!(q.ok(&["show", "1"]).contains("running"));
    q.ok(&["recover"]);
    assert!(q.ok(&["show", "1"]).contains("failed"));
    q.ok(&["run"]);
    q.ok(&[
        "add",
        "--attempts",
        "1",
        "--",
        "/bin/sh",
        "-c",
        "kill -TERM $$",
    ]);
    q.ok(&["run"]);
    assert!(q.ok(&["show", "2"]).contains("terminated by signal"));
}

#[test]
fn storage_errors_do_not_acknowledge_mutations() {
    let q = Queue::new();
    q.ok(&["list"]);
    fs::create_dir(q.root.join("queue/state.new")).unwrap();
    q.fail(&["add", "--", "/usr/bin/true"], "directory");
    assert_eq!(q.ok(&["list"]).lines().count(), 1);
    fs::remove_dir(q.root.join("queue/state.new")).unwrap();
    q.ok(&["add", "--", "/usr/bin/true"]);
    assert!(q.ok(&["show", "1"]).contains("pending"));
    let state = q.root.join("queue/state");
    fs::remove_file(&state).unwrap();
    fs::create_dir(&state).unwrap();
    q.fail(&["list"], "directory");
    let file = q.root.join("not-dir");
    fs::write(&file, b"x").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_durable-spool"))
        .arg("--queue")
        .arg(file.join("child"))
        .arg("list")
        .output()
        .unwrap();
    assert!(!output.status.success());
}

fn rehash(bytes: &mut [u8]) {
    let end = bytes.len() - 8;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in &bytes[..end] {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    bytes[end..].copy_from_slice(&hash.to_le_bytes());
}

#[test]
fn invalid_but_checksummed_snapshots_are_rejected() {
    let q = Queue::new();
    q.ok(&["add", "--", "/usr/bin/true"]);
    let path = q.root.join("queue/state");
    let original = fs::read(&path).unwrap();
    // Public v1 format: magic(8), next-id(8), count(8), id(8), status(1),
    // attempts/max/backoff/ready(4*8), cwd field, result field, argc, argv fields.
    let cwd_len = u64::from_le_bytes(original[65..73].try_into().unwrap()) as usize;
    let result_len_offset = 73 + cwd_len;
    let argc_offset = result_len_offset + 8;
    let arg_len_offset = argc_offset + 8;
    for (offset, replacement) in [
        (0, vec![b'X']),
        (8, 99u64.to_le_bytes().to_vec()),
        (16, 10_001u64.to_le_bytes().to_vec()),
        (24, 7u64.to_le_bytes().to_vec()),
        (32, vec![255]),
        (33, 101u64.to_le_bytes().to_vec()),
        (41, 0u64.to_le_bytes().to_vec()),
        (49, 60_001u64.to_le_bytes().to_vec()),
        (65, u64::MAX.to_le_bytes().to_vec()),
        (73, vec![0]),
        (argc_offset, 0u64.to_le_bytes().to_vec()),
        (arg_len_offset, 1000u64.to_le_bytes().to_vec()),
        (arg_len_offset + 8, vec![0]),
    ] {
        let mut bad = original.clone();
        bad[offset..offset + replacement.len()].copy_from_slice(&replacement);
        rehash(&mut bad);
        fs::write(&path, bad).unwrap();
        q.fail(&["list"], "corrupt");
    }
    let mut trailing = original[..original.len() - 8].to_vec();
    trailing.extend_from_slice(&[0; 9]);
    rehash(&mut trailing);
    fs::write(&path, trailing).unwrap();
    q.fail(&["list"], "corrupt");
    let mut empty_program = original.clone();
    let arg_len = u64::from_le_bytes(
        empty_program[arg_len_offset..arg_len_offset + 8]
            .try_into()
            .unwrap(),
    ) as usize;
    empty_program.drain(arg_len_offset + 8..arg_len_offset + 8 + arg_len);
    empty_program[arg_len_offset..arg_len_offset + 8].copy_from_slice(&0u64.to_le_bytes());
    rehash(&mut empty_program);
    fs::write(&path, empty_program).unwrap();
    q.fail(&["list"], "corrupt");
}

#[test]
fn missing_cwd_is_a_bounded_execution_failure() {
    let q = Queue::new();
    let cwd = q.root.join("gone");
    fs::create_dir(&cwd).unwrap();
    let output = q
        .command()
        .current_dir(&cwd)
        .args(["add", "--attempts", "1", "--", "/usr/bin/true"])
        .output()
        .unwrap();
    assert!(output.status.success());
    fs::remove_dir(&cwd).unwrap();
    q.ok(&["run"]);
    let show = q.ok(&["show", "1"]);
    assert!(show.contains("failed") && show.contains("spawn:"));
}

#[test]
fn cli_missing_arguments_and_default_queue() {
    let q = Queue::new();
    for args in [
        vec![],
        vec!["--queue"],
        vec!["--queue", "x"],
        vec!["--help", "x"],
    ] {
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_durable-spool"))
                .current_dir(&q.root)
                .args(args)
                .output()
                .unwrap()
                .status
                .success()
        );
    }
    for args in [
        vec!["add", "--attempts"],
        vec!["add", "--attempts", "x", "--", "true"],
        vec!["add", "--backoff-ms", "-1", "--", "true"],
        vec!["add", "--", ""],
        vec!["add"],
        vec!["cancel", "x"],
        vec!["recover", "x"],
    ] {
        assert!(!q.output(&args).status.success(), "{args:?}");
    }
    assert!(q.command().arg("--help").output().unwrap().status.success());
    assert!(
        Command::new(env!("CARGO_BIN_EXE_durable-spool"))
            .current_dir(&q.root)
            .args(["list"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(q.root.join(".spool").is_dir());
}

fn snapshot_fixture(jobs: usize, argument_size: usize) -> Vec<u8> {
    let mut bytes = b"DSPOOL01".to_vec();
    bytes.extend_from_slice(&(jobs as u64 + 1).to_le_bytes());
    bytes.extend_from_slice(&(jobs as u64).to_le_bytes());
    for id in 1..=jobs {
        bytes.extend_from_slice(&(id as u64).to_le_bytes());
        bytes.push(0);
        for value in [0u64, 3, 0, 0] {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        // cwd "/", empty result, argc=1, one nonempty program argument.
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.push(b'/');
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        bytes.extend_from_slice(&(argument_size as u64).to_le_bytes());
        bytes.resize(bytes.len() + argument_size, b'x');
    }
    bytes.extend_from_slice(&0u64.to_le_bytes());
    rehash(&mut bytes);
    bytes
}

#[test]
fn capacity_limits_reject_without_losing_existing_jobs() {
    let q = Queue::new();
    q.ok(&["list"]);
    let path = q.root.join("queue/state");
    // Real valid on-disk inputs, generated directly to avoid 10k fsyncs in CI.
    fs::write(&path, snapshot_fixture(10_000, 1)).unwrap();
    q.fail(&["add", "--", "/usr/bin/true"], "10000");
    assert_eq!(q.ok(&["list"]).lines().count(), 10_001);
    // 64 jobs with a one-MiB argument are too large overall. First exercise the
    // decoder bound, then trim the final field to an exactly-full valid snapshot.
    let mut full = snapshot_fixture(64, 1024 * 1024);
    fs::write(&path, &full).unwrap();
    q.fail(&["list"], "corrupt");
    let excess = full.len() - 64 * 1024 * 1024;
    let field_start = full.len() - 8 - 1024 * 1024 - 8;
    full[field_start..field_start + 8]
        .copy_from_slice(&(1024u64 * 1024 - excess as u64).to_le_bytes());
    full.truncate(full.len() - 8 - excess);
    full.extend_from_slice(&0u64.to_le_bytes());
    rehash(&mut full);
    fs::write(&path, &full).unwrap();
    q.fail(&["add", "--", "/usr/bin/true"], "64 MiB");
    assert_eq!(fs::read(&path).unwrap(), full);
}

#[test]
fn filesystem_permission_errors_are_not_empty_queues() {
    use std::os::unix::fs::PermissionsExt;
    let q = Queue::new();
    q.ok(&["add", "--", "/usr/bin/true"]);
    let path = q.root.join("queue/state");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let output = q.output(&["list"]);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    // Root bypasses discretionary permissions; this OS error is unreachable there.
    let uid = Command::new("/usr/bin/id").arg("-u").output().unwrap();
    let is_root = String::from_utf8_lossy(&uid.stdout).trim() == "0";
    if !is_root {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Permission denied"));
    }
    let parent = q.root.join("readonly");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o500)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_durable-spool"))
        .arg("--queue")
        .arg(parent.join("queue"))
        .arg("list")
        .output()
        .unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    if !is_root {
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("Permission denied"));
    }
}

#[test]
fn embedded_nul_in_cwd_is_rejected() {
    let q = Queue::new();
    q.ok(&["add", "--", "/usr/bin/true"]);
    let path = q.root.join("queue/state");
    let mut bytes = fs::read(&path).unwrap();
    bytes[74] = 0; // Keep the leading slash valid so the embedded-NUL check is reached.
    rehash(&mut bytes);
    fs::write(path, bytes).unwrap();
    q.fail(&["list"], "corrupt");
}

#[test]
fn cancellation_races_with_claim_without_lost_state() {
    let q = Queue::new();
    q.ok(&["list"]);
    for round in 0..24 {
        let marker = format!("executed-{round}");
        let id = q.ok(&["add", "--", "/usr/bin/touch", &marker]);
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(q.root.join("queue/state.lock"))
            .unwrap();
        lock.lock().unwrap();
        let mut worker = q.worker(&["run", "--once"]);
        // Queue real lock contenders; no test hooks, altered production timing,
        // or fake lock implementation. The accepted outcome is linearizable.
        std::thread::sleep(Duration::from_millis(10));
        let cancel = q
            .command()
            .args(["cancel", id.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));
        drop(lock);
        let cancelled = cancel.wait_with_output().unwrap().status.success();
        assert!(worker.wait().unwrap().success());
        assert_eq!(q.root.join(&marker).exists(), !cancelled);
        let state = q.ok(&["show", id.trim()]);
        assert!(state.contains(if cancelled { "cancelled" } else { "succeeded" }));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn excessive_argument_count_is_rejected() {
    let q = Queue::new();
    let output = q
        .command()
        .args(["add", "--", "/usr/bin/true"])
        .args(std::iter::repeat_n("", 65_536))
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("65536"));
}

// Unforced OS failures (disk full, fsync/rename/write errors after a successful open,
// descriptor exhaustion, failed wait, poisoned hardware) are not simulated with
// mocks. Real power-loss/storage-controller ordering needs a separate crash rig.
// The encoder's >1-MiB-field guard cannot be reached through this CLI on the
// supported systems: exec argument/path limits are smaller and the decoder rejects
// oversized fields before mutation. macOS ARG_MAX also prevents the 65k-argv case.
// Cancellation/claim and competing directory-creation branches depend on OS
// scheduling; the race workload checks real outcomes without promising all timings.
// APFS rejects non-UTF-8 directory names (EILSEQ); exercise this on Linux.
#[cfg(target_os = "linux")]
#[test]
fn non_utf8_working_directory_is_preserved() {
    let q = Queue::new();
    let dir = q.root.join(OsStr::from_bytes(b"dir-\xff"));
    fs::create_dir(&dir).unwrap();
    let output = q
        .command()
        .current_dir(&dir)
        .args(["add", "--", "/bin/pwd"])
        .output()
        .unwrap();
    assert!(output.status.success());
    q.ok(&["run"]);
    let expected = [dir.canonicalize().unwrap().as_os_str().as_bytes(), b"\n"].concat();
    assert_eq!(
        fs::read(q.root.join("queue/logs/1.1.stdout")).unwrap(),
        expected
    );
}
