use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::net::Ipv4Addr;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
mod vm;

#[cfg(unix)]
pub use vm::{
    CommandResult, DEFAULT_COMMAND_TIMEOUT, DEFAULT_RUNTIME, MAX_COMMAND_TIMEOUT, MAX_FILE_BYTES,
    Vm, VmClient,
};

pub const MAX_INSTANCES: usize = 64;
pub const MAX_SSH_PUBLIC_KEY_BYTES: usize = 512;
const MIN_API_KEY_BYTES: usize = 32;
const MAX_API_KEY_BYTES: usize = 256;
const MAX_WORKLOAD: u64 = 16 * 1024 * 1024;
const MAX_RESPONSE: u64 = 8 * 1024 * 1024;
const DEFAULT_ENDPOINT_PORT: u16 = 8080;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CREATE_SESSION_TIMEOUT: Duration = Duration::from_secs(310);
const SSH_ED25519_PREFIX: &[u8] = b"ssh-ed25519 ";
const SSH_ED25519_BASE64_BYTES: usize = 68;
const SSH_ED25519_BLOB_BYTES: usize = 51;

pub struct RunRequest {
    endpoint: String,
    api_key: String,
    runtime: String,
    workload: PathBuf,
    instances: usize,
    concurrency: usize,
}

impl RunRequest {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        runtime: impl Into<String>,
        workload: impl Into<PathBuf>,
        instances: usize,
        concurrency: usize,
    ) -> io::Result<Self> {
        let endpoint = normalize_endpoint(endpoint.into())?;
        let api_key = api_key.into();
        if !valid_api_key(&api_key) {
            return Err(invalid("API key must contain 32..=256 visible ASCII bytes"));
        }
        let runtime = runtime.into();
        if !is_runtime(&runtime) {
            return Err(invalid("runtime identifier is invalid"));
        }
        if !(1..=MAX_INSTANCES).contains(&instances) {
            return Err(invalid(format!(
                "instance count must be between 1 and {MAX_INSTANCES}"
            )));
        }
        if !(1..=instances).contains(&concurrency) {
            return Err(invalid(
                "concurrency must be between 1 and the instance count",
            ));
        }
        Ok(Self {
            endpoint,
            api_key,
            runtime,
            workload: workload.into(),
            instances,
            concurrency,
        })
    }

    pub fn host(&self) -> &str {
        &self.endpoint
    }

    pub fn instances(&self) -> usize {
        self.instances
    }

    pub fn concurrency(&self) -> usize {
        self.concurrency
    }
}

#[derive(Clone)]
pub struct VmResult {
    pub index: usize,
    pub queue_wait: Duration,
    pub cow_fork: Duration,
    pub restore: Duration,
    pub ready: Duration,
    pub workload_send: Duration,
    pub result_wait: Duration,
    pub teardown: Duration,
    pub output: String,
}

#[derive(Clone, Copy)]
pub struct TemplateAdmission {
    pub load: Duration,
    pub verify: Duration,
    pub prewarm: Duration,
    pub verified_bytes: u64,
}

pub enum Event {
    Phase(String),
    ArtifactReady(Duration),
    WorkloadLoaded(Duration),
    TemplateAdmitted(TemplateAdmission),
    Vm(VmResult),
    Done(Duration),
}

#[derive(Deserialize)]
struct ArtifactResponse {
    artifact_sha256: String,
}

#[derive(Serialize)]
struct ApiRunRequest<'a> {
    artifact_sha256: &'a str,
    runtime: &'a str,
    instances: usize,
    concurrency: usize,
}

#[derive(Deserialize)]
struct ApiRunResponse {
    artifact_sha256: String,
    runtime: String,
    concurrency: usize,
    template_id: String,
    core_sha256: String,
    workload_load_ns: u64,
    template_load_ns: u64,
    template_verify_ns: u64,
    template_prewarm_ns: u64,
    template_verified_bytes: u64,
    vms: Vec<ApiVmResult>,
}

#[derive(Deserialize)]
struct ApiVmResult {
    index: usize,
    queue_wait_ns: u64,
    cow_fork_ns: u64,
    restore_ns: u64,
    guest_ready_ns: u64,
    workload_send_ns: u64,
    result_wait_ns: u64,
    teardown_ns: u64,
    output: String,
}

#[derive(Deserialize)]
struct ErrorResponse {
    error: String,
}

#[derive(Clone)]
pub struct SessionClient {
    endpoint: String,
    api_key: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Ready,
    Failed,
    Destroyed,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session_id: String,
    pub state: SessionState,
    pub runtime: String,
    pub template_id: String,
    pub core_sha256: String,
    pub guest_ipv4: Ipv4Addr,
    pub ssh_port: u16,
    pub ssh_username: String,
    pub ssh_host_public_key: String,
    pub guest_ready_ns: u64,
    pub network_ready_ns: u64,
    pub ssh_ready_ns: u64,
}

#[derive(Serialize)]
struct ApiCreateSessionRequest<'a> {
    runtime: &'a str,
    client_public_key: &'a str,
}

impl SessionClient {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> io::Result<Self> {
        let endpoint = normalize_endpoint(endpoint.into())?;
        let api_key = api_key.into();
        if !valid_api_key(&api_key) {
            return Err(invalid("API key must contain 32..=256 visible ASCII bytes"));
        }
        Ok(Self { endpoint, api_key })
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn uses_http_endpoint(&self) -> bool {
        is_url(&self.endpoint)
    }

    pub fn uses_local_http_endpoint(&self) -> bool {
        reqwest::Url::parse(&self.endpoint).is_ok_and(|url| {
            url.scheme() == "http"
                && matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        })
    }

    pub fn create(&self, runtime: &str, client_public_key: &str) -> io::Result<Session> {
        if !is_runtime(runtime) {
            return Err(invalid("runtime identifier is invalid"));
        }
        if !valid_ssh_public_key(client_public_key) {
            return Err(invalid(
                "client public key is not one strict ssh-ed25519 key",
            ));
        }
        let connection = Connection::open(&self.endpoint, &self.api_key, CREATE_SESSION_TIMEOUT)?;
        let body = serde_json::to_vec(&ApiCreateSessionRequest {
            runtime,
            client_public_key,
        })
        .map_err(other)?;
        let response = connection
            .client
            .post(format!("{}/v0/sessions", connection.base_url))
            .bearer_auth(&connection.api_key)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(other)?;
        let session: Session = decode(response)?;
        validate_session(&session)?;
        if session.runtime != runtime || session.state != SessionState::Ready {
            return Err(invalid("endpoint returned an inconsistent created session"));
        }
        Ok(session)
    }

    pub fn get(&self, id: &str) -> io::Result<Session> {
        if !valid_session_id(id) {
            return Err(invalid("session ID is invalid"));
        }
        let connection = Connection::open(&self.endpoint, &self.api_key, DEFAULT_REQUEST_TIMEOUT)?;
        let response = connection
            .client
            .get(format!("{}/v0/sessions/{id}", connection.base_url))
            .bearer_auth(&connection.api_key)
            .send()
            .map_err(other)?;
        let session: Session = decode(response)?;
        validate_session(&session)?;
        if session.session_id != id {
            return Err(invalid("endpoint returned a different session ID"));
        }
        Ok(session)
    }

    pub fn destroy(&self, id: &str) -> io::Result<()> {
        if !valid_session_id(id) {
            return Err(invalid("session ID is invalid"));
        }
        let connection = Connection::open(&self.endpoint, &self.api_key, DEFAULT_REQUEST_TIMEOUT)?;
        let response = connection
            .client
            .delete(format!("{}/v0/sessions/{id}", connection.base_url))
            .bearer_auth(&connection.api_key)
            .send()
            .map_err(other)?;
        if response.status().is_success() || response.status() == StatusCode::NOT_FOUND {
            Ok(())
        } else {
            Err(response_error(response)?)
        }
    }
}

fn validate_session(session: &Session) -> io::Result<()> {
    if !valid_session_id(&session.session_id)
        || !is_runtime(&session.runtime)
        || !is_sha256(&session.template_id)
        || !is_sha256(&session.core_sha256)
        || !session.guest_ipv4.is_private()
        || session.ssh_port != 22
        || session.ssh_username != "jio"
        || !valid_ssh_public_key(&session.ssh_host_public_key)
        || session.guest_ready_ns == 0
        || session.network_ready_ns < session.guest_ready_ns
        || session.ssh_ready_ns < session.network_ready_ns
    {
        return Err(invalid("endpoint returned inconsistent session metadata"));
    }
    Ok(())
}

pub fn valid_session_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub fn valid_ssh_public_key(value: &str) -> bool {
    if value.len() != SSH_ED25519_PREFIX.len() + SSH_ED25519_BASE64_BYTES
        || value.len() > MAX_SSH_PUBLIC_KEY_BYTES
        || !value.as_bytes().starts_with(SSH_ED25519_PREFIX)
    {
        return false;
    }
    let Some(decoded) = decode_ed25519_base64(&value.as_bytes()[SSH_ED25519_PREFIX.len()..]) else {
        return false;
    };
    decoded[..4] == 11_u32.to_be_bytes()
        && decoded[4..15] == *b"ssh-ed25519"
        && decoded[15..19] == 32_u32.to_be_bytes()
}

fn decode_ed25519_base64(encoded: &[u8]) -> Option<[u8; SSH_ED25519_BLOB_BYTES]> {
    if encoded.len() != SSH_ED25519_BASE64_BYTES {
        return None;
    }
    let mut decoded = [0_u8; SSH_ED25519_BLOB_BYTES];
    for (source, target) in encoded.chunks_exact(4).zip(decoded.chunks_exact_mut(3)) {
        let first = base64_value(source[0])?;
        let second = base64_value(source[1])?;
        let third = base64_value(source[2])?;
        let fourth = base64_value(source[3])?;
        target[0] = (first << 2) | (second >> 4);
        target[1] = (second << 4) | (third >> 2);
        target[2] = (third << 6) | fourth;
    }
    Some(decoded)
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

pub fn run(request: &RunRequest, mut emit: impl FnMut(Event) -> io::Result<()>) -> io::Result<()> {
    let started = Instant::now();
    let workload = read_workload(&request.workload)?;
    let digest = format!("{:x}", Sha256::digest(&workload));

    emit(Event::Phase(format!("Connecting to {}", request.endpoint)))?;
    let connection =
        Connection::open(&request.endpoint, &request.api_key, DEFAULT_REQUEST_TIMEOUT)?;

    emit(Event::Phase("Preparing workload artifact".into()))?;
    let artifact_started = Instant::now();
    connection.prepare_artifact(&digest, workload)?;
    emit(Event::ArtifactReady(artifact_started.elapsed()))?;

    emit(Event::Phase("Admitting and restoring template".into()))?;
    let result = connection.run(
        &digest,
        &request.runtime,
        request.instances,
        request.concurrency,
    )?;
    validate(
        &result,
        &digest,
        &request.runtime,
        request.instances,
        request.concurrency,
    )?;
    emit(Event::WorkloadLoaded(Duration::from_nanos(
        result.workload_load_ns,
    )))?;
    emit(Event::TemplateAdmitted(TemplateAdmission {
        load: Duration::from_nanos(result.template_load_ns),
        verify: Duration::from_nanos(result.template_verify_ns),
        prewarm: Duration::from_nanos(result.template_prewarm_ns),
        verified_bytes: result.template_verified_bytes,
    }))?;
    for vm in result.vms {
        emit(Event::Vm(VmResult {
            index: vm.index,
            queue_wait: Duration::from_nanos(vm.queue_wait_ns),
            cow_fork: Duration::from_nanos(vm.cow_fork_ns),
            restore: Duration::from_nanos(vm.restore_ns),
            ready: Duration::from_nanos(vm.guest_ready_ns),
            workload_send: Duration::from_nanos(vm.workload_send_ns),
            result_wait: Duration::from_nanos(vm.result_wait_ns),
            teardown: Duration::from_nanos(vm.teardown_ns),
            output: vm.output,
        }))?;
    }
    emit(Event::Done(started.elapsed()))
}

struct Connection {
    base_url: String,
    api_key: String,
    client: Client,
    _tunnel: Option<Tunnel>,
}

impl Connection {
    fn open(endpoint: &str, api_key: &str, timeout: Duration) -> io::Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(timeout)
            .build()
            .map_err(other)?;
        let (base_url, tunnel) = if is_url(endpoint) {
            (endpoint.trim_end_matches('/').to_owned(), None)
        } else {
            let tunnel = Tunnel::open(endpoint, endpoint_port()?)?;
            (
                format!("http://127.0.0.1:{}", tunnel.local_port),
                Some(tunnel),
            )
        };
        Ok(Self {
            base_url,
            api_key: api_key.into(),
            client,
            _tunnel: tunnel,
        })
    }

    fn prepare_artifact(&self, digest: &str, workload: Vec<u8>) -> io::Result<()> {
        let cached = self
            .client
            .head(format!("{}/v0/artifacts/{digest}", self.base_url))
            .bearer_auth(&self.api_key)
            .send()
            .map_err(other)?;
        if cached.status().is_success() {
            return Ok(());
        }
        if cached.status() != StatusCode::NOT_FOUND {
            return Err(response_error(cached)?);
        }
        let response = self
            .client
            .put(format!("{}/v0/artifacts/{digest}", self.base_url))
            .bearer_auth(&self.api_key)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(workload)
            .send()
            .map_err(other)?;
        let stored: ArtifactResponse = decode(response)?;
        if stored.artifact_sha256 != digest {
            return Err(invalid("endpoint acknowledged a different artifact digest"));
        }
        Ok(())
    }

    fn run(
        &self,
        digest: &str,
        runtime: &str,
        instances: usize,
        concurrency: usize,
    ) -> io::Result<ApiRunResponse> {
        let body = serde_json::to_vec(&ApiRunRequest {
            artifact_sha256: digest,
            runtime,
            instances,
            concurrency,
        })
        .map_err(other)?;
        let response = self
            .client
            .post(format!("{}/v0/runs", self.base_url))
            .bearer_auth(&self.api_key)
            .header(CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .map_err(other)?;
        decode(response)
    }
}

struct Tunnel {
    child: Child,
    local_port: u16,
}

impl Tunnel {
    fn open(host: &str, remote_port: u16) -> io::Result<Self> {
        let socket = TcpListener::bind(("127.0.0.1", 0))?;
        let local_port = socket.local_addr()?.port();
        drop(socket);
        let forward = format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}");
        let mut child = Command::new("ssh")
            .arg("-N")
            .arg("-T")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("ConnectTimeout=5")
            .arg("-o")
            .arg("ExitOnForwardFailure=yes")
            .arg("-L")
            .arg(forward)
            .arg(host)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let address = std::net::SocketAddr::from(([127, 0, 0, 1], local_port));
        for _ in 0..250 {
            if let Some(status) = child.try_wait()? {
                return Err(io::Error::other(format!(
                    "SSH tunnel exited before it was ready: {status}"
                )));
            }
            if TcpStream::connect_timeout(&address, Duration::from_millis(20)).is_ok() {
                return Ok(Self { child, local_port });
            }
            thread::sleep(Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "SSH tunnel did not become ready",
        ))
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn validate(
    response: &ApiRunResponse,
    digest: &str,
    runtime: &str,
    instances: usize,
    concurrency: usize,
) -> io::Result<()> {
    if response.artifact_sha256 != digest
        || response.runtime != runtime
        || response.concurrency != concurrency
        || !is_sha256(&response.template_id)
        || !is_sha256(&response.core_sha256)
        || response.template_prewarm_ns > response.template_verify_ns
        || response.template_verify_ns > response.template_load_ns
        || response.template_verified_bytes == 0
        || response.vms.len() != instances
    {
        return Err(invalid("endpoint returned inconsistent run provenance"));
    }
    let mut seen = vec![false; instances];
    for vm in &response.vms {
        let index = vm
            .index
            .checked_sub(1)
            .filter(|index| *index < instances)
            .ok_or_else(|| invalid("endpoint returned an invalid VM index"))?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(invalid("endpoint returned a duplicate VM index"));
        }
    }
    Ok(())
}

fn decode<T: for<'de> Deserialize<'de>>(mut response: Response) -> io::Result<T> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Err(invalid("endpoint response exceeded its bound"));
    }
    if !status.is_success() {
        let message = serde_json::from_slice::<ErrorResponse>(&bytes)
            .map(|response| response.error)
            .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
        return Err(io::Error::other(format!(
            "endpoint returned {status}: {message}"
        )));
    }
    serde_json::from_slice(&bytes).map_err(invalid)
}

fn response_error(mut response: Response) -> io::Result<io::Error> {
    let status = response.status();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(MAX_RESPONSE + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RESPONSE {
        return Ok(invalid("endpoint response exceeded its bound"));
    }
    let message = serde_json::from_slice::<ErrorResponse>(&bytes)
        .map(|response| response.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
    Ok(io::Error::other(format!(
        "endpoint returned {status}: {message}"
    )))
}

fn read_workload(path: &PathBuf) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_WORKLOAD + 1)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() as u64 > MAX_WORKLOAD {
        return Err(invalid(format!(
            "workload size is outside the supported range: {}",
            path.display()
        )));
    }
    Ok(bytes)
}

fn normalize_endpoint(endpoint: String) -> io::Result<String> {
    if is_url(&endpoint) {
        let parsed = reqwest::Url::parse(&endpoint).map_err(invalid)?;
        if !matches!(parsed.scheme(), "http" | "https")
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err(invalid("endpoint URL contains unsupported components"));
        }
        if parsed.scheme() == "http"
            && !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "::1"))
        {
            return Err(invalid("plain HTTP is only allowed for a loopback URL"));
        }
        return Ok(endpoint.trim_end_matches('/').into());
    }
    normalize_host(endpoint)
}

fn normalize_host(host: String) -> io::Result<String> {
    if host.is_empty() || host.bytes().filter(|byte| *byte == b'@').count() > 1 {
        return Err(invalid("host is invalid"));
    }
    let host = if host.contains('@') {
        host
    } else {
        format!("ubuntu@{host}")
    };
    let (user, address) = host
        .split_once('@')
        .ok_or_else(|| invalid("host is invalid"))?;
    if user.is_empty()
        || address.is_empty()
        || user.starts_with('-')
        || address.starts_with('-')
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".@:_-[]".contains(&byte))
    {
        return Err(invalid("host contains unsupported characters"));
    }
    Ok(host)
}

fn endpoint_port() -> io::Result<u16> {
    match env::var("JIO_ENDPOINT_PORT") {
        Ok(value) => value.parse().map_err(invalid),
        // Preserve the experimental hosted-Server variable while callers move
        // to endpoint-neutral standalone Core or hosted service configuration.
        Err(env::VarError::NotPresent) => match env::var("JIO_SERVER_PORT") {
            Ok(value) => value.parse().map_err(invalid),
            Err(env::VarError::NotPresent) => Ok(DEFAULT_ENDPOINT_PORT),
            Err(error) => Err(invalid(error)),
        },
        Err(error) => Err(invalid(error)),
    }
}

fn is_url(endpoint: &str) -> bool {
    endpoint.starts_with("http://") || endpoint.starts_with("https://")
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn is_runtime(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte)
        })
}

fn valid_api_key(value: &str) -> bool {
    (MIN_API_KEY_BYTES..=MAX_API_KEY_BYTES).contains(&value.len())
        && value.as_bytes().iter().all(u8::is_ascii_graphic)
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn other(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::{
        ApiRunResponse, ApiVmResult, RunRequest, SessionClient, normalize_endpoint,
        valid_ssh_public_key, validate,
    };
    use std::io;

    const API_KEY: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn normalizes_a_bare_host() -> io::Result<()> {
        let request = RunRequest::new("192.168.1.82", API_KEY, "native-elf-v0", "program", 5, 3)?;
        assert_eq!(request.host(), "ubuntu@192.168.1.82");
        assert_eq!(request.instances(), 5);
        assert_eq!(request.concurrency(), 3);
        assert_eq!(request.runtime, "native-elf-v0");
        Ok(())
    }

    #[test]
    fn accepts_an_https_endpoint() -> io::Result<()> {
        assert_eq!(
            normalize_endpoint("https://api.jio.dev/".into())?,
            "https://api.jio.dev"
        );
        Ok(())
    }

    #[test]
    fn identifies_only_loopback_plain_http_as_local_guest_access() -> io::Result<()> {
        assert!(SessionClient::new("http://127.0.0.1:8080", API_KEY)?.uses_local_http_endpoint());
        assert!(!SessionClient::new("https://api.jio.dev", API_KEY)?.uses_local_http_endpoint());
        Ok(())
    }

    #[test]
    fn rejects_remote_plain_http() {
        assert!(normalize_endpoint("http://api.jio.dev".into()).is_err());
    }

    #[test]
    fn rejects_shell_characters_in_a_host() {
        assert!(RunRequest::new("host;reboot", API_KEY, "native-elf-v0", "program", 1, 1).is_err());
        assert!(
            RunRequest::new(
                "-oProxyCommand=x@host",
                API_KEY,
                "native-elf-v0",
                "program",
                1,
                1,
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_an_invalid_runtime() {
        assert!(RunRequest::new("host", API_KEY, "../python", "program", 1, 1).is_err());
    }

    #[test]
    fn rejects_concurrency_above_the_instance_count() {
        assert!(RunRequest::new("host", API_KEY, "native-elf-v0", "program", 5, 6).is_err());
    }

    #[test]
    fn rejects_duplicate_results() {
        let vm = || ApiVmResult {
            index: 1,
            queue_wait_ns: 1,
            cow_fork_ns: 1,
            restore_ns: 1,
            guest_ready_ns: 1,
            workload_send_ns: 1,
            result_wait_ns: 1,
            teardown_ns: 1,
            output: String::new(),
        };
        let response = ApiRunResponse {
            artifact_sha256: "a".repeat(64),
            runtime: "native-elf-v0".into(),
            concurrency: 1,
            template_id: "b".repeat(64),
            core_sha256: "c".repeat(64),
            workload_load_ns: 1,
            template_load_ns: 3,
            template_verify_ns: 2,
            template_prewarm_ns: 1,
            template_verified_bytes: 134_217_900,
            vms: vec![vm(), vm()],
        };
        assert!(validate(&response, &"a".repeat(64), "native-elf-v0", 2, 1).is_err());
    }

    #[test]
    fn rejects_inconsistent_template_admission_metrics() {
        let response = ApiRunResponse {
            artifact_sha256: "a".repeat(64),
            runtime: "native-elf-v0".into(),
            concurrency: 1,
            template_id: "b".repeat(64),
            core_sha256: "c".repeat(64),
            workload_load_ns: 1,
            template_load_ns: 1,
            template_verify_ns: 2,
            template_prewarm_ns: 3,
            template_verified_bytes: 0,
            vms: vec![ApiVmResult {
                index: 1,
                queue_wait_ns: 1,
                cow_fork_ns: 1,
                restore_ns: 1,
                guest_ready_ns: 1,
                workload_send_ns: 1,
                result_wait_ns: 1,
                teardown_ns: 1,
                output: String::new(),
            }],
        };
        assert!(validate(&response, &"a".repeat(64), "native-elf-v0", 1, 1).is_err());
    }

    #[test]
    fn accepts_only_a_canonical_ed25519_public_key() {
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f";
        assert!(valid_ssh_public_key(key));
        assert!(!valid_ssh_public_key(&format!("restrict {key}")));
        assert!(!valid_ssh_public_key(&format!("{key} comment")));
        let mut malformed = key.as_bytes().to_vec();
        malformed[20] = b'!';
        assert!(!valid_ssh_public_key(&String::from_utf8_lossy(&malformed)));
    }
}
