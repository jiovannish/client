//! One authenticated request per bounded hosted command; never replay commands.
use crate::*;

#[derive(Serialize)]
pub(crate) struct ExecRequest<'a> {
    request_id: &'a str,
    generation: Option<u64>,
    command: &'a str,
    timeout_ms: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecOutput {
    request_id: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    output_limit: bool,
}
impl ExecOutput {
    fn result(self, id: &str) -> io::Result<vm::CommandResult> {
        if self.request_id != id
            || self.stdout.len().saturating_add(self.stderr.len()) > 65536
            || (self.exit_code.is_some() && self.signal.is_some())
        {
            return Err(invalid("invalid exec response"));
        }
        if self.timed_out {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "guest command timed out",
            ));
        }
        if self.output_limit {
            return Err(invalid("guest command exceeded 64 KiB of output"));
        }
        let exit_code = match (self.exit_code, self.signal) {
            (Some(code), None) => code,
            (None, Some(signal)) if (1..=64).contains(&signal) => 128 + signal,
            _ => return Err(invalid("invalid exec exit status")),
        };
        Ok(vm::CommandResult {
            exit_code,
            stdout: self.stdout,
            stderr: self.stderr,
        })
    }
}
fn request<'a>(
    id: &'a str,
    command: &'a str,
    timeout: Duration,
    generation: Option<u64>,
) -> io::Result<ExecRequest<'a>> {
    let timeout_ms = u32::try_from(timeout.as_millis()).map_err(other)?;
    if command.is_empty()
        || command.len() > 4096
        || command.contains('\0')
        || !(1..=30_000).contains(&timeout_ms)
        || generation == Some(0)
    {
        return Err(invalid(
            "direct exec requires a command <=4096 bytes and timeout 1..30000 ms",
        ));
    }
    Ok(ExecRequest {
        request_id: id,
        generation,
        command,
        timeout_ms,
    })
}
impl SessionClient {
    /// Executes in one HTTP request. No SSH handshake or capability preflight.
    /// A lost response has an unknown outcome: this method never retries.
    pub fn exec_direct(
        &self,
        id: &str,
        generation: Option<u64>,
        command: &str,
        timeout: Duration,
    ) -> io::Result<vm::CommandResult> {
        if !valid_session_id(id) {
            return Err(invalid("invalid session ID"));
        }
        let nonce = vm::random_session_id()?;
        let request = request(&nonce, command, timeout, generation)?;
        let connection = self.connection()?;
        let response = connection
            .client
            .post(format!("{}/v0/sessions/{id}/exec", connection.base_url))
            .bearer_auth(&connection.api_key)
            .timeout(timeout + Duration::from_secs(10))
            .header(CONTENT_TYPE, "application/json")
            .body(serde_json::to_vec(&request).map_err(other)?)
            .send()
            .map_err(other)?;
        if response.status() == StatusCode::NOT_IMPLEMENTED {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "template has no direct exec capability",
            ));
        }
        decode::<ExecOutput>(response)?.result(&nonce)
    }
    pub(crate) fn create_and_exec(
        &self,
        id: &str,
        key: &str,
        size: VmSize,
        command: &str,
        timeout: Duration,
    ) -> io::Result<(Session, vm::CommandResult)> {
        let nonce = vm::random_session_id()?;
        let request = request(&nonce, command, timeout, None)?;
        let connection = self.connection()?;
        let response = connection.client.post(format!("{}/v0/sessions/exec",connection.base_url))
            .bearer_auth(&connection.api_key).timeout(CREATE_SESSION_TIMEOUT + timeout)
            .header(CONTENT_TYPE, "application/json").body(serde_json::to_vec(&serde_json::json!({"session_id":id,"client_public_key":key,"size":size,"exec":request})).map_err(other)?).send().map_err(other)?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Created {
            session: Session,
            output: ExecOutput,
        }
        let created: Created = decode(response)?;
        validate_session(&created.session)?;
        if created.session.session_id != id
            || created.session.size != Some(size)
            || created.session.state != SessionState::Ready
        {
            return Err(invalid("create-exec returned a different session"));
        }
        Ok((created.session, created.output.result(&nonce)?))
    }
}
