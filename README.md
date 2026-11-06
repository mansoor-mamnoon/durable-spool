# durable-spool

**Queue commands now. Run them later. Know what a crash means.**

A small, dependency-free Rust command spool for one Unix machine. It stores work
before acknowledging it, runs one command at a time, and keeps stdout/stderr for
each attempt. No Redis, database server, daemon, or socket.

Useful for local exports, media processing, build batches, and other **idempotent**
jobs that should outlive the terminal that submitted them. Submission is durable;
execution still needs a foreground worker, a terminal multiplexer, or your service
manager.

## Start here

Requires Rust 1.89+ on Unix. Tested locally on macOS; Linux is covered by CI.

```sh
git clone https://github.com/mansoor-mamnoon/durable-spool
cd durable-spool
cargo install --path .
durable-spool --queue "$HOME/my-work" add -- /usr/bin/printf '%s\n' 'hello world'
# Prints the durable job ID: 1
durable-spool --queue "$HOME/my-work" run
durable-spool --queue "$HOME/my-work" show 1
cat "$HOME/my-work/logs/1.1.stdout"
```

A successful `add` means the snapshot file and its containing directory have been
synchronized. This relies on the filesystem and hardware honoring those requests;
it is not a claim of tested physical power-loss resilience.

Arguments are stored as separate byte strings: spaces, empty arguments, Unicode,
non-UTF-8 Unix arguments, and leading dashes survive. **No implicit shell.**
The submitting working directory is saved. The environment, PATH, executable
contents, and input files are **not** snapshotted; execution uses the worker's
environment. Use absolute executable paths for predictable lookup. Stdin is closed.

## Commands

| Command | Behavior |
| --- | --- |
| `add [--attempts N] [--backoff-ms N] -- PROGRAM [ARG...]` | Persist a command; print ID |
| `run` | Drain pending work, waiting for retry delays; exit when none remains |
| `run --once` | Perform at most one currently eligible attempt; never wait for a delay |
| `list` | Show status, attempts/budget, next eligible Unix timestamp in milliseconds |
| `show ID` | Show escaped argv/cwd, last result, and log directory |
| `cancel ID` | Cancel pending work; never kill a running process |
| `recover` | Explicitly authorize retry of interrupted work, subject to its remaining budget |

Put `--queue DIR` before the command. The default is `.spool` in the current
directory, so use an absolute queue path across terminals.

Known failures retry up to **3 total attempts** by default, at 1s then 2s.
Configure 1–100 total attempts and a 0–60,000ms initial delay; exponential delays
cap at 60 seconds. Each process receives `DURABLE_SPOOL_JOB_ID` and
`DURABLE_SPOOL_ATTEMPT`. IDs are unique only within a queue; combine an
application-specific queue identifier with the job ID when designing idempotency keys.

`run` exits zero when it successfully drains the queue, **even if jobs failed**.
Check `list`/`show` for outcomes. CLI, storage, and worker-contention errors exit 1.

## Crash recovery, without pretending it is exactly-once

```text
pending --durable claim--> running --successful exit + durable ack--> succeeded
   |                         |
 cancel                 known failure --> pending (backoff) or failed
   |                         |
cancelled                worker crash --> running (outcome unknown)
                                           |
                                   explicit recover
                                           |
                                  pending or failed
```

After a worker dies, the next `run` refuses to replay interrupted jobs silently.
Inspect logs and external effects; **stop or wait for orphaned children first**.
Then run `recover`, followed by `run`. Recovery itself never executes commands.

A command can complete an external effect just before its worker crashes.
Retrying may repeat it. A crash between claim and spawn may consume an attempt
without executing anything. This is an **at-least-once retry model with a finite
budget**, not guaranteed eventual delivery and never exactly-once effects.
Use idempotent operations, transactional sinks, or application-level deduplication.

## Why another queue?

[Pueue](https://github.com/Nukesor/pueue) is the better choice for rich interactive
process management, groups, and dependencies. [Task Spooler](https://viric.name/soft/ts/)
already solves local batch scheduling. This is not a claim that those tools are
broken.

durable-spool explores a narrower combination: **no daemon, no third-party
dependencies, byte-preserving argv, and an explicit decision at the uncertain
execution boundary**. The storage protocol is small enough to explain and audit.
See [DESIGN.md](DESIGN.md) for the protocol, tradeoffs, research, and interview guide.

## Boundaries

- One worker per queue, concurrent submitters; no distributed execution or priority/DAG scheduling.
- Trusted local filesystem and trusted queue owner only. No NFS/SMB, hostile directory writers, or symlink hardening.
- Whole-state snapshots: O(queue size) per mutation, at most 10,000 retained jobs / 64 MiB state. Start a new queue for a new batch; no pruning yet.
- Logs have no quota or rotation. Watch disk space.
- No timeout, process-tree termination, environment capture, or continuous daemon mode. Detached grandchildren are outside the job lifecycle.
- A crash can leave a child alive. The worker lock excludes workers, not surviving command trees.
- No physical power-cut tests or throughput claims. Do not use this MVP for payments or other non-idempotent critical actions.

## Verify

```sh
cargo test -- --test-threads=10
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo tree  # just durable-spool: zero dependencies
```

All tests are real CLI/filesystem/process tests, including SIGKILL recovery,
concurrent producers, worker exclusion, exact argv, retry limits, pending
cancellation, corrupt snapshots, and log/storage failures. No mocks.

For a runnable crash-recovery demonstration:

```sh
cargo test --test e2e killed_worker_requires_explicit_recovery_and_preserves_attempt_budget -- --exact --nocapture
```

Local macOS measurement: 18 applicable E2E tests passed; production-source coverage
was 97.84% of lines and 95% of branches using nightly LLVM instrumentation. Two
additional tests target Linux (non-UTF-8 paths and its larger argument budget).
This is not a 100%-coverage claim: OS error paths, argument-size limits, and rare
scheduling branches remain outside the measured coverage.
