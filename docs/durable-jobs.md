# Durable command jobs

The Rust client and CLI support explicit durable steps on an existing owned session
with native exec. This requires Server's durable-job API; standalone Core and older
Server releases do not implement these endpoints. Node/Python job bindings are not
included yet. Normal VM APIs remain available in those SDKs.

Save one immutable plan and one stable idempotency key before submitting. Reuse both
after a lost submission response. The plan uses a caller-selected 32-character
lowercase hexadecimal job ID, an existing session ID/generation, a future deadline
within 24 hours, and 1–16 uniquely named ordered steps. Commands total at most 16 KiB,
each command is at most 4096 bytes and 30000 ms, and results contain at most 64 KiB
combined output. The full JSON plan must fit 32 KiB. Each account retains at most
128 jobs including completed results; no automatic deletion discards deduplication.

```json
{
  "job_id": "0123456789abcdef0123456789abcdef",
  "session_id": "11111111111111111111111111111111",
  "generation": 1,
  "deadline_unix_seconds": 1792843200,
  "not_before_unix_seconds": 0,
  "steps": [
    {"step_id": "prepare", "command": "printf prepared", "timeout_ms": 1000},
    {"step_id": "run", "command": "printf result", "timeout_ms": 1000}
  ]
}
```

Set the deadline to a future Unix timestamp, then:

```sh
jio job submit plan.json stable-job-submission
jio job run 0123456789abcdef0123456789abcdef prepare 1
jio job status 0123456789abcdef0123456789abcdef
jio job run 0123456789abcdef0123456789abcdef run 1
```

Commands return JSON progress/results. Calling a completed step again returns its
saved outcome, including while the VM is stopped. A lost step reply is not permission
to start another job or replay the command: query the same job/step. `uncertain`
retains the original attempt and blocks subsequent work. The first protocol has one
attempt per step. It does not automatically retry failed or uncertain commands.

`jio job cancel <id>` prevents further dispatch; `cancel_requested` means an existing
attempt remains unresolved. Cancellation does not promise to undo an external effect
or terminate a running guest process immediately. The native timeout still bounds it.
Stored wake times and deadlines are checked during explicit advancement; there is
no automatic background scheduler in this slice.

After a clean retained stop/start of the same logical session, use the new generation:

```sh
jio job bind <job-id> <same-session-id> <new-generation> <current-epoch>
```

Bind requires a clean step boundary and advances the epoch. Use that returned epoch
for subsequent steps. Different-session binding is rejected because this API does
not yet have a qualified recovery reference proving the destination's disk lineage.
It does not perform a VM move, import private Core packets, or establish cross-host
recovery. A job's completed output alone does not prove guest filesystem durability;
programs must flush required data and recovery must use a qualified storage boundary.

Rust uses the same implementation as the CLI:

```rust,no_run
use jio_client::{SessionClient, jobs::{JobPlan, JobState}};

# fn main() -> std::io::Result<()> {
let api = SessionClient::new("https://your-endpoint", "your-api-key-at-least-32-bytes-long")?;
let plan: JobPlan = serde_json::from_slice(&std::fs::read("plan.json")?)?;
let job = api.create_job("stable-job-submission", &plan)?;
let progress = api.run_job_step(&job.job_id, "prepare", job.epoch)?;
if progress.state == JobState::Uncertain {
    // Reconcile the existing attempt; do not replay its external actions.
}
let saved = api.job(&job.job_id)?;
# let _ = saved;
# Ok(())
# }
```

The typed response distinguishes job state, step state, attempt identity and stable
logical `effect_key`. `jobs::effect_key(job_id, step_id)` derives that key before
submission when a tool/API needs an idempotency token. It is effective only if the
external target honors it. Arbitrary shell commands are not rewritten and external
side effects are not guaranteed exactly once.

Production promotion still requires native Core/session recovery, real public
placement/routing, fault/power-loss and supported language-binding acceptance.
Local HTTP/PostgreSQL tests qualify their named boundaries only. Review-time
optimization candidates include database round trips per step, result serialization
and API polling frequency; measure those separately from VM start and agent execution
before changing correctness or durability boundaries.
