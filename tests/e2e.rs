#![cfg(unix)]
use std::ffi::{OsStr, OsString};
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
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tmp").join(format!(
            "e2e-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_durable-spool"));
        command.arg("--queue").arg(self.root.join("queue")).current_dir(&self.root);
        command
    }

    fn output(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let output = self.output(args);
        assert!(output.status.success(), "{args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    }

    fn fail(&self, args: &[&str], needle: &str) {
        let output = self.output(args);
        assert!(!output.status.success(), "{args:?} unexpectedly succeeded");
        assert!(String::from_utf8_lossy(&output.stderr).contains(needle), "{:?}", output);
    }

    fn worker(&self, args: &[&str]) -> Child {
        self.command().args(args).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap()
    }
}

impl Drop for Queue {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.root); }
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "timed out waiting for {}", path.display());
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn executes_exact_arguments_and_cwd_with_logs() {
    let q = Queue::new();
    let strange = OsString::from_vec(vec![b'x', 0xff, b' ', b'y']);
    let output = q.command().args(["add", "--", "/usr/bin/printf", "%s\\n"])
        .arg("").arg("a b").arg("$(touch injected)").arg("--flag").arg(&strange).output().unwrap();
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "1");
    q.ok(&["run"]);
    let mut expected = b"\na b\n$(touch injected)\n--flag\n".to_vec();
    expected.extend_from_slice(strange.as_bytes());
    expected.push(b'\n');
    assert_eq!(fs::read(q.root.join("queue/logs/1.1.stdout")).unwrap(), expected);
    assert!(!q.root.join("injected").exists());
    assert!(q.ok(&["show", "1"]).contains("succeeded"));
    q.ok(&["add", "--", "/bin/pwd"]);
    q.ok(&["run", "--once"]);
    let cwd = fs::read(q.root.join("queue/logs/2.1.stdout")).unwrap();
    assert_eq!(cwd, [q.root.canonicalize().unwrap().as_os_str().as_bytes(), b"\n"].concat());
}

#[test]
fn retries_are_bounded_and_spawn_failure_is_recorded() {
    let q = Queue::new();
    q.ok(&["add", "--attempts", "3", "--backoff-ms", "1", "--", "/bin/sh", "-c", "echo no >&2; exit 7"]);
    q.ok(&["run"]);
    let show = q.ok(&["show", "1"]);
    assert!(show.contains("failed") && show.contains("3/3") && show.contains("exit 7"), "{show}");
    assert_eq!(fs::read(q.root.join("queue/logs/1.3.stderr")).unwrap(), b"no\n");
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
    q.fail(&["add", "--backoff-ms", "99999999", "--", "true"], "backoff");
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
    let mut producers: Vec<_> = (0..32).map(|_| {
        q.command().args(["add", "--", "/usr/bin/true"]).stdout(Stdio::piped()).spawn().unwrap()
    }).collect();
    let mut ids = Vec::new();
    for producer in producers.drain(..) {
        let output = producer.wait_with_output().unwrap();
        assert!(output.status.success());
        ids.push(String::from_utf8(output.stdout).unwrap().trim().parse::<u64>().unwrap());
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
    assert!(Command::new("/bin/kill").args(["-KILL", pid.trim()]).status().unwrap().success());
    q.fail(&["run"], "recover");
    q.ok(&["recover"]);
    q.ok(&["run"]);
    let show = q.ok(&["show", "1"]);
    assert!(show.contains("succeeded") && show.contains("2/2"), "{show}");
    assert_eq!(fs::read(q.root.join("queue/logs/1.2.stdout")).unwrap(), b"recovered\n");
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
fn non_utf8_working_directory_is_preserved() {
    let q = Queue::new();
    let dir = q.root.join(OsStr::from_bytes(b"dir-\xff"));
    fs::create_dir(&dir).unwrap();
    let output = q.command().current_dir(&dir).args(["add", "--", "/bin/pwd"]).output().unwrap();
    assert!(output.status.success());
    q.ok(&["run"]);
    let expected = [dir.canonicalize().unwrap().as_os_str().as_bytes(), b"\n"].concat();
    assert_eq!(fs::read(q.root.join("queue/logs/1.1.stdout")).unwrap(), expected);
}
