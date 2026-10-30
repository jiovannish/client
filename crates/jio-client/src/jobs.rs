//! Durable bounded command steps. Completion is cached; uncertainty is never replayed.
//!
//! A job pins an existing session generation. Each step has one attempt in this
//! protocol. Saving files or starting services does not preserve process memory or
//! make arbitrary external side effects exactly once.
use crate::*;
use std::collections::BTreeSet;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobStep {
    pub step_id: String,
    pub command: String,
    pub timeout_ms: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobPlan {
    pub job_id: String,
    pub session_id: String,
    pub generation: u64,
    pub deadline_unix_seconds: u64,
    pub not_before_unix_seconds: u64,
    pub steps: Vec<JobStep>,
}

impl JobPlan {
    /// Validate stable input bounds. Server time decides whether deadlines admit work.
    pub fn validate(&self) -> io::Result<()> {
        if !valid_session_id(&self.job_id)
            || !valid_session_id(&self.session_id)
            || !(1..=i64::MAX as u64).contains(&self.generation)
            || self.deadline_unix_seconds > i64::MAX as u64
            || self.not_before_unix_seconds >= self.deadline_unix_seconds
            || self.steps.is_empty()
            || self.steps.len() > 16
        {
            return Err(invalid(
                "invalid job identity, generation, deadline or step count",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut bytes = 0_usize;
        for step in &self.steps {
            if !label(&step.step_id)
                || !ids.insert(&step.step_id)
                || step.command.is_empty()
                || step.command.len() > 4096
                || step.command.contains('\0')
                || !(1..=30000).contains(&step.timeout_ms)
            {
                return Err(invalid("invalid or duplicate job step"));
            }
            bytes = bytes
                .checked_add(step.command.len())
                .ok_or_else(|| invalid("job command size overflow"))?;
        }
        if bytes > 16 * 1024 {
            return Err(invalid("job commands exceed 16 KiB"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Ready,
    Running,
    Succeeded,
    Failed,
    Uncertain,
    CancelRequested,
    Cancelled,
    Expired,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Dispatched,
    Completed,
    Failed,
    Uncertain,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobOutput {
    pub request_id: String,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub output_limit: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct JobStepView {
    pub step_id: String,
    pub state: StepState,
    pub attempt_id: Option<String>,
    pub attempt_epoch: Option<u64>,
    pub effect_key: String,
    pub result: Option<JobOutput>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub job_id: String,
    pub session_id: String,
    pub generation: u64,
    pub epoch: u64,
    pub state: JobState,
    pub deadline_unix_seconds: u64,
    pub not_before_unix_seconds: u64,
    pub steps: Vec<JobStepView>,
}

/// Stable logical effect identity across attempts and session generations.
/// External systems must explicitly honor it; commands are not automatically rewritten.
pub fn effect_key(job_id: &str, step_id: &str) -> io::Result<String> {
    if !valid_session_id(job_id) || !label(step_id) {
        return Err(invalid("invalid job or step ID"));
    }
    Ok(format!(
        "{:x}",
        Sha256::digest(format!("durable-job-effect-v1\n{job_id}\n{step_id}").as_bytes())
    ))
}

fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b))
}

impl Job {
    fn validate(&self, expected_id: &str) -> io::Result<()> {
        if self.job_id != expected_id
            || !valid_session_id(&self.job_id)
            || !valid_session_id(&self.session_id)
            || self.generation == 0
            || self.generation > i64::MAX as u64
            || !(1..=1024).contains(&self.epoch)
            || self.not_before_unix_seconds >= self.deadline_unix_seconds
            || self.steps.is_empty()
            || self.steps.len() > 16
        {
            return Err(invalid("invalid job response"));
        }
        let mut ids = BTreeSet::new();
        let mut pending = false;
        let mut unresolved = 0;
        for step in &self.steps {
            if !label(&step.step_id)
                || !ids.insert(&step.step_id)
                || step.effect_key != effect_key(&self.job_id, &step.step_id)?
            {
                return Err(invalid("invalid job step identity"));
            }
            if step.state == StepState::Pending {
                pending = true;
                if step.attempt_id.is_some()
                    || step.attempt_epoch.is_some()
                    || step.result.is_some()
                {
                    return Err(invalid("pending step has attempt state"));
                }
                continue;
            }
            if pending
                || !step.attempt_id.as_deref().is_some_and(valid_session_id)
                || !step.attempt_epoch.is_some_and(|e| e > 0 && e <= self.epoch)
            {
                return Err(invalid("invalid attempt identity or order"));
            }
            let finished = matches!(step.state, StepState::Completed | StepState::Failed);
            if finished != step.result.is_some() {
                return Err(invalid("invalid durable step result"));
            }
            if matches!(step.state, StepState::Dispatched | StepState::Uncertain) {
                unresolved += 1;
            }
            if let Some(out) = &step.result {
                let has_status = out.exit_code.is_some()
                    || out.signal.is_some()
                    || out.timed_out
                    || out.output_limit;
                let success = out.exit_code == Some(0)
                    && out.signal.is_none()
                    && !out.timed_out
                    && !out.output_limit;
                if Some(out.request_id.as_str()) != step.attempt_id.as_deref()
                    || out.stdout.len().saturating_add(out.stderr.len()) > 65536
                    || out.exit_code.is_some() && out.signal.is_some()
                    || !has_status
                    || out.signal.is_some_and(|n| !(1..=64).contains(&n))
                    || (step.state == StepState::Completed) != success
                {
                    return Err(invalid("invalid command outcome"));
                }
            }
        }
        if unresolved > 1
            || (matches!(
                self.state,
                JobState::Ready
                    | JobState::Succeeded
                    | JobState::Failed
                    | JobState::Cancelled
                    | JobState::Expired
            ) && unresolved != 0)
            || (matches!(
                self.state,
                JobState::Running | JobState::Uncertain | JobState::CancelRequested
            ) && unresolved != 1)
            || (self.state == JobState::Succeeded
                && self.steps.iter().any(|s| s.state != StepState::Completed))
        {
            return Err(invalid("inconsistent job and step state"));
        }
        Ok(())
    }
}

impl SessionClient {
    /// Persist a bounded immutable plan under an account-wide idempotency key.
    /// Retry the exact plan/key after a lost response; do not invent another job ID.
    pub fn create_job(&self, key: &str, plan: &JobPlan) -> io::Result<Job> {
        plan.validate()?;
        if !label(key) {
            return Err(invalid("invalid job idempotency key"));
        }
        let connection = self.connection()?;
        let response = connection
            .client
            .post(format!("{}/v1/jobs", connection.base_url))
            .bearer_auth(&connection.api_key)
            .header("idempotency-key", key)
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(plan).map_err(other)?)
            .send()
            .map_err(other)?;
        let job: Job = decode(response)?;
        job.validate(&plan.job_id)?;
        if job.session_id != plan.session_id
            || job
                .steps
                .iter()
                .map(|s| &s.step_id)
                .ne(plan.steps.iter().map(|s| &s.step_id))
        {
            return Err(invalid("endpoint returned a different job binding or plan"));
        }
        Ok(job)
    }

    /// Read durable progress/results even when the bound VM is offline.
    pub fn job(&self, id: &str) -> io::Result<Job> {
        self.job_request(id, None, None)
    }

    /// Request one dispatch attempt for a logical step; execution may not start.
    /// Duplicate calls return the attempt's durable state/result.
    /// `Uncertain` is an outcome to reconcile, never an instruction to replay work.
    pub fn run_job_step(&self, id: &str, step: &str, expected_epoch: u64) -> io::Result<Job> {
        if !label(step) || !(1..=1024).contains(&expected_epoch) {
            return Err(invalid("invalid step or epoch"));
        }
        let job = self.job_request(
            id,
            Some(&format!("steps/{step}/run")),
            Some(serde_json::json!({"expected_epoch":expected_epoch})),
        )?;
        if job.epoch != expected_epoch || !job.steps.iter().any(|s| s.step_id == step) {
            return Err(invalid("endpoint returned a different step or epoch"));
        }
        Ok(job)
    }

    /// Prevent future dispatch; an in-flight command can remain unresolved.
    pub fn cancel_job(&self, id: &str) -> io::Result<Job> {
        self.job_request(id, Some("cancel"), Some(serde_json::json!({})))
    }

    /// Resume the same logical session after an authorized retained restart.
    /// Different-session recovery and unresolved attempts are rejected by Server.
    pub fn bind_job(
        &self,
        id: &str,
        expected_epoch: u64,
        session_id: &str,
        generation: u64,
    ) -> io::Result<Job> {
        if !valid_session_id(session_id)
            || !(1..1024).contains(&expected_epoch)
            || generation == 0
            || generation > i64::MAX as u64
        {
            return Err(invalid("invalid job binding"));
        }
        let job = self.job_request(id,Some("bind"),Some(serde_json::json!({"expected_epoch":expected_epoch,"session_id":session_id,"generation":generation})))?;
        if job.session_id != session_id
            || job.generation != generation
            || job.epoch != expected_epoch + 1
        {
            return Err(invalid("endpoint returned a different job binding"));
        }
        Ok(job)
    }

    fn job_request(
        &self,
        id: &str,
        action: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> io::Result<Job> {
        if !valid_session_id(id) {
            return Err(invalid("invalid job ID"));
        }
        let connection = self.connection()?;
        let url = match action {
            Some(action) => format!("{}/v1/jobs/{id}/{action}", connection.base_url),
            None => format!("{}/v1/jobs/{id}", connection.base_url),
        };
        let mut request = connection
            .client
            .request(
                if action.is_some() {
                    reqwest::Method::POST
                } else {
                    reqwest::Method::GET
                },
                url,
            )
            .bearer_auth(&connection.api_key)
            .timeout(Duration::from_secs(45));
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body).map_err(other)?);
        }
        let job: Job = decode(request.send().map_err(other)?)?;
        job.validate(id)?;
        Ok(job)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, Write};

    #[test]
    fn rejects_inconsistent_progress_and_pins_effect_identity() -> io::Result<()> {
        let id = "a".repeat(32);
        let mut job = Job {
            job_id: id.clone(),
            session_id: "b".repeat(32),
            generation: 1,
            epoch: 1,
            state: JobState::Ready,
            deadline_unix_seconds: 200,
            not_before_unix_seconds: 0,
            steps: vec![JobStepView {
                step_id: "one".into(),
                state: StepState::Pending,
                attempt_id: None,
                attempt_epoch: None,
                effect_key: effect_key(&id, "one")?,
                result: None,
            }],
        };
        job.validate(&id)?;
        job.steps[0].attempt_id = Some("c".repeat(32));
        assert!(job.validate(&id).is_err());
        job.steps[0].attempt_id = None;
        job.state = JobState::Succeeded;
        assert!(job.validate(&id).is_err());
        job.state = JobState::Ready;
        job.steps[0].effect_key = "d".repeat(64);
        assert!(job.validate(&id).is_err());
        assert!(effect_key(&id, "../escape").is_err());
        Ok(())
    }

    fn completed(id: &str) -> io::Result<serde_json::Value> {
        Ok(
            serde_json::json!({"job_id":id,"session_id":"b".repeat(32),"generation":1,"epoch":1,"state":"succeeded","deadline_unix_seconds":200,"not_before_unix_seconds":0,"steps":[{"step_id":"one","state":"completed","attempt_id":"c".repeat(32),"attempt_epoch":1,"effect_key":effect_key(id,"one")?,"result":{"request_id":"c".repeat(32),"stdout":[100,111,110,101],"stderr":[],"exit_code":0,"signal":null,"timed_out":false,"output_limit":false}}]}),
        )
    }

    fn read_request(stream: &TcpStream) -> io::Result<(String, serde_json::Value)> {
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut reader = io::BufReader::new(stream);
        let mut headers = String::new();
        loop {
            let mut line = String::new();
            if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                break;
            }
            headers.push_str(&line);
            if headers.len() > 8192 {
                return Err(invalid("test headers oversized"));
            }
        }
        assert!(
            headers
                .to_ascii_lowercase()
                .contains(&format!("authorization: bearer {}", "a".repeat(64)))
        );
        let len = headers
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length: ")
                    .map(str::to_owned)
            })
            .map(|s| s.parse::<usize>().map_err(other))
            .transpose()?
            .unwrap_or(0);
        if len > 32768 {
            return Err(invalid("test body oversized"));
        }
        let mut bytes = vec![0; len];
        reader.read_exact(&mut bytes)?;
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).map_err(other)?
        };
        Ok((headers, body))
    }

    #[test]
    fn transport_preserves_ids_keys_epochs_and_returns_cached_outcomes() -> io::Result<()> {
        let id = "a".repeat(32);
        let plan = JobPlan {
            job_id: id.clone(),
            session_id: "b".repeat(32),
            generation: 1,
            deadline_unix_seconds: 200,
            not_before_unix_seconds: 0,
            steps: vec![JobStep {
                step_id: "one".into(),
                command: "printf done".into(),
                timeout_ms: 1000,
            }],
        };
        let result = completed(&id)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let expected_plan = serde_json::to_value(&plan).map_err(other)?;
        let server = thread::spawn(move || -> io::Result<()> {
            for (method, path, body) in [
                ("POST", "/v1/jobs".to_owned(), expected_plan),
                (
                    "POST",
                    format!("/v1/jobs/{id}/steps/one/run"),
                    serde_json::json!({"expected_epoch":1}),
                ),
                (
                    "POST",
                    format!("/v1/jobs/{id}/steps/one/run"),
                    serde_json::json!({"expected_epoch":1}),
                ),
                ("GET", format!("/v1/jobs/{id}"), serde_json::Value::Null),
                (
                    "POST",
                    format!("/v1/jobs/{id}/cancel"),
                    serde_json::json!({}),
                ),
            ] {
                let (mut stream, _) = listener.accept()?;
                let (headers, actual) = read_request(&stream)?;
                assert!(headers.starts_with(&format!("{method} {path} HTTP/1.1\r\n")));
                assert_eq!(actual, body);
                if path == "/v1/jobs" {
                    assert!(
                        headers
                            .to_ascii_lowercase()
                            .contains("idempotency-key: stable-submit")
                    );
                }
                let bytes = result.to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{bytes}",
                    bytes.len()
                )?;
            }
            Ok(())
        });
        let api = SessionClient::new(endpoint, "a".repeat(64))?;
        let job = api.create_job("stable-submit", &plan)?;
        assert_eq!(job.state, JobState::Succeeded);
        for _ in 0..2 {
            assert_eq!(
                api.run_job_step(&plan.job_id, "one", 1)?.state,
                JobState::Succeeded
            );
        }
        assert_eq!(
            api.job(&plan.job_id)?.steps[0]
                .result
                .as_ref()
                .map(|r| r.stdout.as_slice()),
            Some(b"done".as_slice())
        );
        assert_eq!(api.cancel_job(&plan.job_id)?.state, JobState::Succeeded);
        server
            .join()
            .map_err(|_| io::Error::other("test HTTP server failed"))??;
        Ok(())
    }

    #[test]
    fn lost_reply_is_returned_without_an_automatic_step_replay() -> io::Result<()> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let server = thread::spawn(move || -> io::Result<()> {
            let (stream, _) = listener.accept()?;
            read_request(&stream)?;
            drop(stream);
            listener.set_nonblocking(true)?;
            thread::sleep(Duration::from_millis(100));
            assert!(
                listener
                    .accept()
                    .is_err_and(|e| e.kind() == io::ErrorKind::WouldBlock)
            );
            Ok(())
        });
        let api = SessionClient::new(endpoint, "a".repeat(64))?;
        assert!(api.run_job_step(&"b".repeat(32), "one", 1).is_err());
        server
            .join()
            .map_err(|_| io::Error::other("test HTTP server failed"))??;
        Ok(())
    }
}
