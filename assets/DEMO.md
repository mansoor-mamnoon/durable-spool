# A local handoff for an agent

[Watch the 30-second demo](demo.gif) · [Text transcript](demo-transcript.md)

An agent queues an idempotent local report task, then its submitting process
exits. A separately invoked foreground worker executes it. The task deliberately
exits 75 on attempt 1, before writing anything; the worker retries and attempt 2
writes the report. The demo reads both attempt logs and the final report.

There is no hosted LLM, network call, or external service in this demonstration.
“Agent” describes the caller's use case, not an actual model invocation. The
report is a small illustrative artifact, not a claim of repository validation.
The transient failure is explicitly simulated in Rust.
No worker crash, terminal closure, or automatic crash recovery is demonstrated.
Pending jobs survive their submitter; they do not execute without a worker.
Run the worker in a separate terminal, terminal multiplexer, or supervised service
if it needs to continue after the submitting terminal closes.

## Reproduce

Requires Rust 1.89+ on Unix and ImageMagick 7 with an installed monospace font.
From the repository root:

```sh
cargo build --bin durable-spool
DEMO_FONT=Courier cargo run --example demo -- tmp/demo-recording-001
```

Use a **new, nonexistent scratch directory** for each recording. All queue data,
the copied Rust task binary, the report, and intermediate PNGs stay there. On
macOS, the checked-in GIF was rendered with:

```sh
DEMO_FONT=/System/Library/Fonts/Menlo.ttc \
  cargo run --example demo -- tmp/demo-recording-002
```

The recorder in [examples/demo.rs](../examples/demo.rs) is also the task
executable: it copies itself to the scratch directory as `agent-task`, then
queues `./agent-task --task`. No shell script or Python dependency is used.
Every visible command output is captured from real child processes, with
assertions checking the job ID, pending state, attempt logs, successful retry
count, and report content. Displayed `durable-spool` commands use the built
`target/debug/durable-spool` executable internally. The `cat` commands are
also executed, not reconstructed from expected strings.

The recorder overwrites `assets/demo.gif` and `assets/demo-transcript.md`.
The transcript retains the raw output (including tab separators); the GIF
expands tabs for readability. Job timestamps vary between recordings.
Section titles and explanatory footers are presentation annotations, not CLI
output. Playback is paced for reading rather than real-time execution: five
scenes held for 5, 5, 7, 7, and 6 seconds, with an infinite loop.

Inspect the result:

```sh
magick identify -format '%s: canvas=%Wx%H delay=%Tcs\n' assets/demo.gif
```

The logical canvas is 960 × 600. Optimized GIF frames after the first may be
smaller rectangles. View the coalesced animation, not isolated delta frames.

## Icon

[icon.svg](icon.svg) is an original, self-contained 128 × 128 vector mark:
three queued tasks and a completion check. It uses no fonts or external assets
and includes an accessible title and description. Both the icon and the demo
are covered by the repository's MIT license.
