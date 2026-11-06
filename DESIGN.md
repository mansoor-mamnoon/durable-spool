# Design and interview guide

## The problem

A local batch queue sounds trivial: save commands and run them. The difficult
part is the gap between **recording an attempt**, **starting a process**,
**performing an external effect**, and **recording completion**. Those events
cannot be one atomic transaction.

The Task Spooler author's motivation is concrete: submit from different terminals,
inspect output and exit status, and avoid running too many RAM/disk-heavy jobs.
Pueue demonstrates that this is an established Rust tool category. The useful
scope here is a small auditable alternative, not a replacement for either tool
or evidence of a large unmet market.

## Architecture

```text
add / cancel / list / show             run / recover
           |                           worker.lock
           |                        (whole invocation)
           +---------------+---------------+
                           |
                       state.lock
                    (one transaction)
                           |
                      load + validate
                           |
                       mutate state
                           |
                 write state.new, sync_all
                           |
                 rename state.new -> state
                           |
                   sync queue directory
                           |
                      acknowledge
```

Locks are OS-managed advisory file locks, not PID files. A killed process
releases its descriptors and therefore its locks. The lock files themselves
are never renamed or removed. All participating processes lock the same inode.

The worker holds its lock across child execution, but releases the state lock
between transactions. Producers, inspectors, and pending cancellations can run
while a command is busy. Only the worker can change a claimed job's outcome.
`recover` also needs the worker lock, so it cannot race a live worker.

### Transaction invariants

1. An acknowledged enqueue has a committed snapshot containing that job.
2. Job IDs are increasing and never reused within a queue.
3. A pending job is durably changed to running, with its attempt count incremented,
   **before** spawning a command.
4. Success is committed only after the foreground child exits successfully and
   its log file handles are synchronized.
5. A mutation reads the latest committed state while holding the exclusive state
   lock: concurrent writers cannot overwrite each other's changes.
6. A corrupted/unsupported snapshot is an error, never an empty queue.
7. A worker finding running records stops and asks for explicit recovery.

The commit sequence is temp write → file sync → same-directory rename →
directory sync. Before rename, an abandoned temporary file is uncommitted and
ignored. After rename, readers see a complete old or new snapshot. A failed
directory sync is reported, even though the rename may already be visible:
**an error does not prove the mutation did not happen**. Re-inspect before
resubmitting. There is no enqueue idempotency token in v0.1.

Directory creation also synchronizes parent entries. These claims assume local
filesystem rename/locking semantics and a storage stack that honors flushes.
They are not a proof against firmware lying, media corruption, or all power-loss
behavior. macOS testing validates process crashes, not removal of power.

## Binary snapshot format (v1)

All integers are unsigned little-endian 64-bit values except the one-byte status.
A byte field is a length followed by that many bytes.

```text
"DSPOOL01" | next_id | job_count
repeated jobs:
  id | status | attempts | max_attempts | backoff_ms | ready_at_ms
  cwd_field | last_result_field | argc | repeated argv_fields
FNV-1a-64 checksum of all preceding bytes
```

Status values: pending=0, running=1, succeeded=2, failed=3, cancelled=4.
Bounds: 64 MiB snapshot, 10,000 jobs, 1 MiB per field, 65,536 arguments,
100 attempts and 60,000ms initial/capped retry delay.
The parser validates lengths, IDs, enum tags, option ranges, absolute cwd,
NUL-free command bytes, and trailing data. The checksum detects accidental damage,
not malicious modification. It is neither authentication nor collision-proof.

An atomic snapshot avoids WAL torn-tail recovery and compaction machinery at the
cost of rewriting the queue on every transition. There is no throughput claim:
for large queues, a proven transactional database is a better foundation.

## Execution and recovery

`Command` receives each argument separately; only an explicit user-supplied shell
interprets scripts. The child's cwd is restored from raw Unix bytes. stdout and
stderr go to `logs/ID.ATTEMPT.stdout` and `.stderr`. There are no pipes to fill
while the parent waits. Logs are not size-bounded.

Known nonzero exits, signals, and spawn errors consume an attempt and schedule
retry or terminal failure. Wait/log/commit errors stop the worker, preserving the
uncertain running record instead of declaring a false failure. Retry eligibility
uses wall-clock milliseconds, while IDs use a counter. Clock jumps can accelerate
or delay retries but cannot duplicate IDs. Eligible jobs are selected in ID order;
a delayed earlier job does not block ready later jobs.

| Crash point | Committed state | Recovery meaning |
| --- | --- | --- |
| Before enqueue rename | Old snapshot | Enqueue was not acknowledged |
| After enqueue rename, before reply | May contain new job | Inspect before resubmitting |
| After claim, before spawn | Running | Attempt consumed, command may never have started |
| During command | Running | Child may still run; inspect/stop/wait before recovery |
| After external effect, before ack | Running | Retry may repeat the effect |
| After durable success ack | Succeeded | No further automatic execution |

Recovery keeps the attempt count. It requeues interrupted work with remaining
budget and backoff; exhausted jobs become failed. It cannot determine whether
the previous command had an effect. The word “failed” means the spool cannot
report successful completion, not that no external effect occurred.

## Explain it in an interview

“I built a local durable command spool in Rust without a database or daemon.
Two locks separate short state transactions from long-running execution. Every
state change writes and syncs a temporary snapshot, atomically renames it, then
syncs the directory. The interesting impossibility is that a child process's
effects and the queue acknowledgment cannot be atomic. I therefore persist a
claim before spawn, preserve uncertain running records after crashes, and require
explicit recovery. Retrying is at-least-once, bounded, and assumes idempotent jobs.”

Follow-up tradeoffs worth discussing:

- Why a stable lock inode matters when the data inode is replaced.
- Why a successful write or rename alone is not a durable acknowledgment.
- Why exactly-once message delivery would not imply exactly-once shell effects.
- Why a WAL or embedded database becomes preferable as queue size grows.
- Why killing the worker does not mean its whole process tree died.

## Research

Visited November 6, 2026. These sources informed the design; they do not endorse
this implementation.

- [Task Spooler](https://viric.name/soft/ts/): author motivation and client/server model.
- [Pueue](https://github.com/Nukesor/pueue): mature Rust local command manager and scope.
- [Terminal Trove / Pueue](https://terminaltrove.com/pueue/): local compilation/download use cases.
- [Rust File](https://doc.rust-lang.org/std/fs/struct.File.html): sync and advisory locking APIs, stable since 1.89 for locks.
- [Linux fsync(2)](https://man7.org/linux/man-pages/man2/fsync.2.html): separately synchronizing directory entries.
- [SQLite atomic commit](https://www.sqlite.org/atomiccommit.html): flush ordering, checksums, and storage assumptions.
- [Redis Streams](https://redis.io/docs/latest/develop/data-types/streams/): pending delivery and acknowledgment separation.
- [AWS SQS at-least-once](https://docs.aws.amazon.com/AWSSimpleQueueService/latest/SQSDeveloperGuide/standard-queues-at-least-once-delivery.html): duplicate delivery and idempotence.
- [Celery tasks](https://docs.celeryq.dev/en/stable/userguide/tasks.html): late acknowledgment, worker loss, bounded retries and backoff.
