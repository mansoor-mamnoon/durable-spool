# CLI benchmarks

These measurements exercise the real `durable-spool` executable, not an in-memory approximation. They measure the cost of submitting and running tiny commands on one laptop. They are not a comparison against another queue and are not a durability or power-loss test.

## Reproduce

```sh
cargo build --release --bin durable-spool --example benchmark
mkdir -p tmp/benchmark
# The last directory must not already exist; use a new name for each run.
target/release/examples/benchmark \
  target/release/durable-spool tmp/benchmark/run-1 10,100,1000 5
```

The harness uses only Rust's standard library and requires Unix and `/usr/bin/true`. It writes `raw.csv`, `summary.csv`, and a separate retained queue for each size/trial into the output directory. Any failed subprocess, unexpected job ID, or unexpected final queue status fails the run. It does not delete queues, so their snapshots and logs remain available for inspection.

`cargo test --test benchmark_e2e` compiles and executes the benchmark against the actual Cargo-built CLI on tiny queues. It checks the CSV statistics, final retained states and log files, invalid arguments, existing-output protection, and subprocess errors. It has no latency or throughput thresholds.

## What is timed

For each of five trials, the harness processes sizes 10, 100, and 1,000 in that order:

1. Launch `/usr/bin/true` directly N times, serially, collecting each spawn/wait duration. This is a separate process-launch baseline, not a value subtracted from queue timings.
2. Starting with a fresh queue, launch `durable-spool add --attempts 1 -- /usr/bin/true` N times, serially. Record each process's end-to-end duration, including process launch, snapshot load/write, synchronization, and response capture. Check every returned ID. The batch duration also includes ID checks and loop overhead.
3. Launch a single `durable-spool run` and time it until exit. All N job records remain in the snapshot throughout draining, even after success. This includes payload launches, claim/completion snapshots, log creation and synchronization, and the worker's final empty-pending scan.
4. Outside the timed region, list the queue and require exactly N records with sequential IDs, `succeeded`, one attempt out of one, and no retry timestamp.

The submission queue grows from zero to N; add latency is a distribution across that growth, **not latency at a fixed queue depth N**. Drain throughput is N jobs divided by the single drain invocation's elapsed time. Submission throughput is N divided by the complete submission loop's elapsed time. Sizes refer to retained job records, not concurrent workers: one worker executes one command at a time.

Timers use `std::time::Instant`. Raw durations are integer nanoseconds; this unit does not imply nanosecond accuracy. Per-trial median and p95 use the nearest-rank definition (`ceil(percentile × count)`, one-based). All raw samples are preserved; no outliers are removed. CSV serialization happens outside timed phases. There is one untimed CLI `help` invocation and one untimed payload invocation before the experiment, but no full queue warmup and no cache flushing. Queue initialization is included in the first submission of every trial.

## Durability path

There are no benchmarking-only changes to the queue. Each mutation writes a whole snapshot, calls `File::sync_all` on it, renames it, and synchronizes the queue directory. The worker synchronizes the logs directory and both log files before acknowledging success. These calls remain enabled in the release build. The actual power-loss guarantees depend on Rust, the OS, filesystem and storage device; a throughput measurement cannot establish them.

## Interpretation and limits

- The entire retained snapshot is parsed and rewritten on mutations. Larger retained queues increase metadata work. This architecture prioritizes a small auditable implementation, not high-volume queue throughput.
- `/usr/bin/true` produces no output and does no useful work. Real agent tasks, network calls, model inference, repository builds, and substantial logs will change both absolute throughput and the fraction spent in queue overhead.
- The direct-spawn baseline has no queue, snapshots, or durable log files. Its descriptor setup differs from the worker's; it is context, not an apples-to-apples competing implementation.
- No parallel producers, multiple queues, retries, crash recovery, network filesystem, sustained soak, or power-loss behavior is measured here.
- This is an ordinary shared laptop, without CPU pinning or an isolated storage device. Background load, caches, thermal state, and power management can affect results. Five trials do not establish a stable tail-latency SLO. At size 10, a per-trial p95 is the maximum of only ten observations.
- Absolute paths and argument lengths are serialized into every record and affect snapshot size. Reproduction from a different working-directory path can therefore change the results.

## Recorded run

Measured November 7, 2026 (UTC), using the release CLI from production source commit `57b2b0e` and benchmark harness commit `637fd65`. All **5,550 submitted jobs succeeded exactly once in these runs**, as reported by the queue's attempt counters; this is not a general exactly-once execution guarantee.

| Jobs per fresh queue | Add median (ms) | Add p95 (ms) | Submit (jobs/s) | Drain (jobs/s) | Direct spawn median (ms) |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 10 | 17.93 | 29.16 | 53.60 | 29.53 | 1.45 |
| 100 | 20.11 | 31.44 | 46.33 | 29.71 | 1.25 |
| 1,000 | 22.28 | 30.61 | 40.59 | 27.73 | 1.02 |

Latency columns pool all five trials at each size (50, 500, and 5,000 observations respectively), then use nearest-rank percentiles. Throughput columns are the median of the five per-trial throughputs, not throughput calculated from pooled latency percentiles. Drain throughput across individual trials ranged from 24.77–38.89 jobs/s at size 10, 27.57–33.02 at size 100, and 26.45–31.30 at size 1,000. This variation is a reason not to treat the table as a capacity guarantee.

**Practical reading:** on this machine, durable CLI submission took around 18–22 ms at the median, and a single worker drained no-op commands at around 28–30 jobs/s. This is a meaningful overhead for tiny commands; for long-running agent tasks it may be a small part of total runtime, but that workload was not benchmarked.

### Machine and build

- Apple M2 Pro, model identifier Mac14,9; 12 physical/logical CPU cores; 16 GiB RAM.
- macOS 26.0 (25A354), Darwin 25.0.0, aarch64.
- APFS on the local internal solid-state Apple Fabric device; 994.7 GB disk, approximately 327.6 GB container free at the start. Queue directories were on the writable Data volume, not a RAM disk.
- `rustc 1.98.0 (88d9e12ae 2026-08-18)`, host `aarch64-apple-darwin`, LLVM 22.1.8.
- Cargo release profile: optimized, LTO enabled, stripped; no custom `RUSTFLAGS` were set by the benchmark commands.
- The recorded working-directory path was 122 bytes long. Every job stored that path and the same `/usr/bin/true` argv.
- System load averages at initial inspection were 4.26 / 3.76 / 3.63; after completion, 3.92 / 3.91 / 3.80. Other work was running on the machine; no isolated-host claim is made.
- Measured CLI SHA-256: `89ce35bab02a79efee903e7101799dcf2c78368123a1319cda77f602f0f989f9`.

### Raw data

- [Every timing sample](benchmarks/raw.csv): 11,115 rows plus the header, comprising 5,550 add samples, 5,550 direct-spawn samples, and 15 whole-drain samples. `index` is one-based for add/spawn and zero for the whole drain.
- [Per-trial summaries](benchmarks/summary.csv): 15 rows plus the header. Durations are nanoseconds, throughput is jobs/second, and `succeeded` is the final verified job count. Per-trial percentile columns are **not** the pooled percentiles shown in the table.

No failures, retries, sample filtering, or timing-based pass criteria were used.
