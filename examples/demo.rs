//! Record the real CLI and render the documentation GIF. See assets/DEMO.md.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn checked(command: &mut Command) -> Output {
    let output = command.output().expect("start command");
    assert!(
        output.status.success(),
        "command failed: {command:?}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn capture(command: &mut Command) -> String {
    let output = checked(command);
    assert!(output.stderr.is_empty(), "unexpected stderr");
    String::from_utf8(output.stdout).expect("UTF-8 demo output")
}

fn cli(binary: &Path, cwd: &Path, args: &[&str]) -> String {
    capture(Command::new(binary).current_dir(cwd).args(args))
}

fn task() {
    // Deliberately simulate a known failure before any external effect.
    if env::var("DURABLE_SPOOL_ATTEMPT").as_deref() == Ok("1") {
        eprintln!("SIMULATED transient failure: local index not ready");
        std::process::exit(75);
    }
    // Repeating this task replaces the same artifact with the same bytes.
    fs::write(
        "agent-report.txt",
        "Local agent report\nDemo artifact ready for the agent.\n",
    )
    .expect("write report");
    println!("Wrote agent-report.txt (safe to repeat)");
}

fn annotate(command: &mut Command, font: &str, size: &str, color: &str, at: &str, text: &str) {
    command.args([
        "-font",
        font,
        "-pointsize",
        size,
        "-fill",
        color,
        "-annotate",
        at,
        text,
    ]);
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.get(1).map(String::as_str) == Some("--task") {
        task();
        return;
    }
    let scratch = PathBuf::from(
        args.get(1)
            .expect("usage: cargo run --example demo -- tmp/UNIQUE"),
    );
    assert!(!scratch.exists(), "choose a fresh scratch directory");
    fs::create_dir_all(&scratch).expect("create scratch");
    let scratch = fs::canonicalize(scratch).expect("absolute scratch");
    let binary = fs::canonicalize("target/debug/durable-spool").expect("cargo build first");
    fs::copy(
        env::current_exe().expect("recorder path"),
        scratch.join("agent-task"),
    )
    .expect("copy task executable");
    let font = env::var("DEMO_FONT").unwrap_or_else(|_| "Courier".into());

    let added = cli(&binary, &scratch, &["add", "--", "./agent-task", "--task"]);
    assert_eq!(added, "1\n");
    let pending = cli(&binary, &scratch, &["list"]);
    assert!(pending.contains("pending\t0/3"));
    // Every CLI invocation is a distinct process. The submitter has exited.
    let later = cli(&binary, &scratch, &["list"]);
    assert_eq!(pending, later);
    let run = cli(&binary, &scratch, &["run"]);
    assert!(run.is_empty());
    let failure = capture(Command::new("cat").arg(scratch.join(".spool/logs/1.1.stderr")));
    assert_eq!(
        failure,
        "SIMULATED transient failure: local index not ready\n"
    );
    let success = capture(Command::new("cat").arg(scratch.join(".spool/logs/1.2.stdout")));
    assert_eq!(success, "Wrote agent-report.txt (safe to repeat)\n");
    let completed = cli(&binary, &scratch, &["list"]);
    assert!(completed.contains("succeeded\t2/3"));
    let report = capture(Command::new("cat").arg(scratch.join("agent-report.txt")));
    assert_eq!(
        report,
        "Local agent report\nDemo artifact ready for the agent.\n"
    );

    let scenes = [
        (
            "01 / QUEUE IT",
            "Your agent hands off a local task.",
            format!(
                "$ durable-spool add -- ./agent-task --task\n{added}\n$ durable-spool list\n{pending}"
            ),
            "A durable job ID. No server to deploy.",
            500,
        ),
        (
            "02 / LEAVE IT",
            "The submitting process has already exited.",
            format!("$ durable-spool list\n{later}"),
            "The job stays queued. A worker must still run.",
            500,
        ),
        (
            "03 / RUN IT",
            "A worker drains the queue, including retries.",
            format!("$ durable-spool run\n\n$ cat .spool/logs/1.1.stderr\n{failure}"),
            "Demo fault: attempt 1 exits 75 before writing anything.",
            700,
        ),
        (
            "04 / CHECK IT",
            "Known failure retried. Second attempt succeeded.",
            format!("$ cat .spool/logs/1.2.stdout\n{success}\n$ durable-spool list\n{completed}"),
            "Per-attempt logs + explicit outcome. Not exactly-once.",
            700,
        ),
        (
            "05 / KEEP THE RESULT",
            "The agent can return for the artifact.",
            format!("$ cat agent-report.txt\n{report}"),
            "Idempotent task: repeat execution replaces the same file.",
            600,
        ),
    ];
    let mut transcript = String::from(
        "# Demo transcript\n\nGenerated from real subprocess outputs by examples/demo.rs.\nCommands use the default .spool in one scratch working directory.\nThe task and simulated first-attempt failure are local Rust code; no LLM or network.\nEach submitting CLI process exits before the next command. No terminal closure or worker crash is simulated.\nThe worker runs in the foreground and returns without stdout; logs are read after it finishes.\nPlayback is paced for reading, not wall-clock timing. Tabs are expanded only in the GIF.\n\n",
    );
    let mut gif = Command::new("magick");
    for (index, (title, subtitle, body, footer, delay)) in scenes.iter().enumerate() {
        transcript.push_str(&format!(
            "## {title}\n\n{subtitle}\n\n```text\n{body}```\n\n{footer}\n\n"
        ));
        let frame = scratch.join(format!("frame-{index}.png"));
        let mut render = Command::new("magick");
        render.args([
            "-size",
            "960x600",
            "xc:#0b1020",
            "-fill",
            "#151e32",
            "-draw",
            "roundrectangle 20,20 940,580 18,18",
            "-fill",
            "#243149",
            "-draw",
            "rectangle 21,78 939,79",
            "-fill",
            "#70e1c1",
            "-draw",
            "circle 47,49 52,49",
        ]);
        annotate(
            &mut render,
            &font,
            "18",
            "#b9c7dd",
            "+66+56",
            "durable-spool   /   local work, durable handoff",
        );
        annotate(&mut render, &font, "16", "#70e1c1", "+46+117", title);
        annotate(&mut render, &font, "24", "#f0f4fc", "+46+158", subtitle);
        let display = body.replace('\t', "    ");
        annotate(&mut render, &font, "18", "#e1e8f5", "+46+222", &display);
        annotate(&mut render, &font, "16", "#70e1c1", "+46+510", footer);
        annotate(
            &mut render,
            &font,
            "15",
            "#94a6c3",
            "+46+550",
            "LOCAL RUST TASK   /   NO HOSTED LLM   /   NO NETWORK",
        );
        render.arg(&frame);
        checked(&mut render);
        gif.args(["-delay", &delay.to_string()]).arg(frame);
    }
    fs::write("assets/demo-transcript.md", transcript).expect("write transcript");
    gif.args(["-loop", "0", "-layers", "Optimize", "assets/demo.gif"]);
    checked(&mut gif);
    println!("Recorded 5 scenes / 30 seconds to assets/demo.gif");
}
