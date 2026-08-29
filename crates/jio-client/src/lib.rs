use reqwest::StatusCode;
use reqwest::blocking::{Client, Response};
use reqwest::header::CONTENT_TYPE;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

pub const MAX_INSTANCES: usize = 64;
const MAX_WORKLOAD: u64 = 16 * 1024 * 1024;
const MAX_RESPONSE: u64 = 8 * 1024 * 1024;
const DEFAULT_SERVER_PORT: u16 = 8080;

pub struct RunRequest {
    endpoint: String,
    api_key: String,
    workload: PathBuf,
    instances: usize,
}

impl RunRequest {
    pub fn new(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        workload: impl Into<PathBuf>,
        instances: usize,
    ) -> io::Result<Self> {
        let endpoint = normalize_endpoint(endpoint.into())?;
        let api_key = api_key.into();
        if api_key.is_empty() {
            return Err(invalid("API key is empty"));
        }
        if !(1..=MAX_INSTANCES).contains(&instances) {
            return Err(invalid(format!(
                "instance count must be between 1 and {MAX_INSTANCES}"
            )));
        }
        Ok(Self {
            endpoint,
            api_key,
            workload: workload.into(),
            instances,
        })
    }

    pub fn host(&self) -> &str {
        &self.endpoint
    }

    pub fn instances(&self) -> usize {
        self.instances
    }
}

#[derive(Clone)]
pub struct VmResult {
    pub index: usize,
    pub cow_fork: Duration,
    pub restore: Duration,
    pub ready: Duration,
    pub workload_send: Duration,
    pub result_wait: Duration,
    pub teardown: Duration,
    pub output: String,
}

pub enum Event {
    Phase(String),
    ArtifactReady(Duration),
    WorkloadLoaded(Duration),
    TemplateLoaded(Duration),
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
    instances: usize,
}

#[derive(Deserialize)]
struct ApiRunResponse {
    artifact_sha256: String,
    template_id: String,
    core_sha256: String,
    workload_load_ns: u64,
    template_load_ns: u64,
    vms: Vec<ApiVmResult>,
}

#[derive(Deserialize)]
struct ApiVmResult {
    index: usize,
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

pub fn run(request: &RunRequest, mut emit: impl FnMut(Event) -> io::Result<()>) -> io::Result<()> {
    let started = Instant::now();
    let workload = read_workload(&request.workload)?;
    let digest = format!("{:x}", Sha256::digest(&workload));

    emit(Event::Phase(format!("Connecting to {}", request.endpoint)))?;
    let connection = Connection::open(&request.endpoint, &request.api_key)?;

    emit(Event::Phase("Preparing ELF artifact".into()))?;
    let artifact_started = Instant::now();
    connection.prepare_artifact(&digest, workload)?;
    emit(Event::ArtifactReady(artifact_started.elapsed()))?;

    emit(Event::Phase("Restoring template".into()))?;
    let result = connection.run(&digest, request.instances)?;
    validate(&result, &digest, request.instances)?;
    emit(Event::WorkloadLoaded(Duration::from_nanos(
        result.workload_load_ns,
    )))?;
    emit(Event::TemplateLoaded(Duration::from_nanos(
        result.template_load_ns,
    )))?;
    for vm in result.vms {
        emit(Event::Vm(VmResult {
            index: vm.index,
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
    fn open(endpoint: &str, api_key: &str) -> io::Result<Self> {
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(other)?;
        let (base_url, tunnel) = if is_url(endpoint) {
            (endpoint.trim_end_matches('/').to_owned(), None)
        } else {
            let tunnel = Tunnel::open(endpoint, server_port()?)?;
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
            return Err(invalid("server acknowledged a different artifact digest"));
        }
        Ok(())
    }

    fn run(&self, digest: &str, instances: usize) -> io::Result<ApiRunResponse> {
        let body = serde_json::to_vec(&ApiRunRequest {
            artifact_sha256: digest,
            instances,
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

fn validate(response: &ApiRunResponse, digest: &str, instances: usize) -> io::Result<()> {
    if response.artifact_sha256 != digest
        || !is_sha256(&response.template_id)
        || !is_sha256(&response.core_sha256)
        || response.vms.len() != instances
    {
        return Err(invalid("server returned inconsistent run provenance"));
    }
    let mut seen = vec![false; instances];
    for vm in &response.vms {
        let index = vm
            .index
            .checked_sub(1)
            .filter(|index| *index < instances)
            .ok_or_else(|| invalid("server returned an invalid VM index"))?;
        if std::mem::replace(&mut seen[index], true) {
            return Err(invalid("server returned a duplicate VM index"));
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
        return Err(invalid("server response exceeded its bound"));
    }
    if !status.is_success() {
        let message = serde_json::from_slice::<ErrorResponse>(&bytes)
            .map(|response| response.error)
            .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
        return Err(io::Error::other(format!(
            "server returned {status}: {message}"
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
        return Ok(invalid("server response exceeded its bound"));
    }
    let message = serde_json::from_slice::<ErrorResponse>(&bytes)
        .map(|response| response.error)
        .unwrap_or_else(|_| String::from_utf8_lossy(&bytes).into_owned());
    Ok(io::Error::other(format!(
        "server returned {status}: {message}"
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
            return Err(invalid("server URL contains unsupported components"));
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

fn server_port() -> io::Result<u16> {
    match env::var("JIO_SERVER_PORT") {
        Ok(value) => value.parse().map_err(invalid),
        Err(env::VarError::NotPresent) => Ok(DEFAULT_SERVER_PORT),
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

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

fn other(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::{ApiRunResponse, ApiVmResult, RunRequest, normalize_endpoint, validate};
    use std::io;

    #[test]
    fn normalizes_a_bare_host() -> io::Result<()> {
        let request = RunRequest::new("192.168.1.81", "key", "program", 5)?;
        assert_eq!(request.host(), "ubuntu@192.168.1.81");
        assert_eq!(request.instances(), 5);
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
    fn rejects_remote_plain_http() {
        assert!(normalize_endpoint("http://api.jio.dev".into()).is_err());
    }

    #[test]
    fn rejects_shell_characters_in_a_host() {
        assert!(RunRequest::new("host;reboot", "key", "program", 1).is_err());
        assert!(RunRequest::new("-oProxyCommand=x@host", "key", "program", 1).is_err());
    }

    #[test]
    fn rejects_duplicate_results() {
        let vm = || ApiVmResult {
            index: 1,
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
            template_id: "b".repeat(64),
            core_sha256: "c".repeat(64),
            workload_load_ns: 1,
            template_load_ns: 1,
            vms: vec![vm(), vm()],
        };
        assert!(validate(&response, &"a".repeat(64), 2).is_err());
    }
}
