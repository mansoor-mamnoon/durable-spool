# Demo transcript

Generated from real subprocess outputs by examples/demo.rs.
Commands use the default .spool in one scratch working directory.
The task and simulated first-attempt failure are local Rust code; no LLM or network.
Each submitting CLI process exits before the next command. No terminal closure or worker crash is simulated.
The worker runs in the foreground and returns without stdout; logs are read after it finishes.
Playback is paced for reading, not wall-clock timing. Tabs are expanded only in the GIF.

## 01 / QUEUE IT

Your agent hands off a local task.

```text
$ durable-spool add -- ./agent-task --task
1

$ durable-spool list
ID	STATUS	ATTEMPTS	READY_AT_MS
1	pending	0/3	0
```

A durable job ID. No server to deploy.

## 02 / LEAVE IT

The submitting process has already exited.

```text
$ durable-spool list
ID	STATUS	ATTEMPTS	READY_AT_MS
1	pending	0/3	0
```

The job stays queued. A worker must still run.

## 03 / RUN IT

A worker drains the queue, including retries.

```text
$ durable-spool run

$ cat .spool/logs/1.1.stderr
SIMULATED transient failure: local index not ready
```

Demo fault: attempt 1 exits 75 before writing anything.

## 04 / CHECK IT

Known failure retried. Second attempt succeeded.

```text
$ cat .spool/logs/1.2.stdout
Wrote agent-report.txt (safe to repeat)

$ durable-spool list
ID	STATUS	ATTEMPTS	READY_AT_MS
1	succeeded	2/3	1794023370505
```

Per-attempt logs + explicit outcome. Not exactly-once.

## 05 / KEEP THE RESULT

The agent can return for the artifact.

```text
$ cat agent-report.txt
Local agent report
Demo artifact ready for the agent.
```

Idempotent task: repeat execution replaces the same file.

