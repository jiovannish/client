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
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};

mod list;
pub use list::VmSummary;

#[cfg(unix)]
mod vm;

#[cfg(unix)]
pub use vm::{
    CommandResult, DEFAULT_COMMAND_TIMEOUT, MAX_COMMAND_TIMEOUT, MAX_FILE_BYTES,
    PreparedConnection, Vm, VmClient,
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
/// Default managed Jio connection. API keys are always supplied separately.
pub const DEFAULT_ENDPOINT: &str = "https://46.105.119.217";
const JIO_CA: &[u8] = include_bytes!("jio-ca.pem");
const SSH_ED25519_PREFIX: &[u8] = b"ssh-ed25519 ";
const SSH_ED25519_BASE64_BYTES: usize = 68;
const SSH_ED25519_BLOB_BYTES: usize = 51;

/// Resolves an explicit connection, environment overrides, or the built-in Jio default.
pub fn resolve_endpoint(explicit: Option<String>) -> io::Result<String> {
    let value = explicit
        .map(std::ffi::OsString::from)
        .or_else(|| env::var_os("JIO_ENDPOINT"))
        .or_else(|| env::var_os("JIO_HOST"))
        .unwrap_or_else(|| DEFAULT_ENDPOINT.into());
    normalize_endpoint(
        value
            .into_string()
            .map_err(|_| invalid("Jio connection is not valid UTF-8"))?,
    )
}

/// A fixed Jio VM resource profile.
///
/// Sizes identify immutable templates; they do not resize a running VM.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum VmSize {
    #[serde(rename = "small")]
    #[default]
    Small,
    #[serde(rename = "medium")]
    Medium,
    #[serde(rename = "large")]
    Large,
    #[serde(rename = "xlarge")]
    XLarge,
}

impl VmSize {
    pub const ALL: [Self; 4] = [Self::Small, Self::Medium, Self::Large, Self::XLarge];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::XLarge => "xlarge",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Small => "Small",
            Self::Medium => "Medium",
            Self::Large => "Large",
            Self::XLarge => "X-Large",
        }
    }

    pub const fn vcpus(self) -> u8 {
        match self {
            Self::Small => 1,
            Self::Medium => 2,
            Self::Large => 4,
            Self::XLarge => 8,
        }
    }

    pub const fn memory_mib(self) -> u32 {
        match self {
            Self::Small => 512,
            Self::Medium => 4 * 1024,
            Self::Large => 8 * 1024,
            Self::XLarge => 16 * 1024,
        }
    }
}

impl std::fmt::Display for VmSize {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.id())
    }
}

impl FromStr for VmSize {
    type Err = io::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "small" => Ok(Self::Small),
            "medium" => Ok(Self::Medium),
            "large" => Ok(Self::Large),
            "xlarge" => Ok(Self::XLarge),
            _ => Err(invalid(format!("unknown VM size: {value}"))),
        }
    }
}

/// Hosted account reservations, not measured utilization or billed consumption.
/// All API keys on the account share these limits.
#[derive(Debug, Deserialize)]
pub struct AccountUsage {
    pub account_id: String,
    pub limits: ResourceUsage,
    pub reserved: ResourceUsage,
    pub compute_sessions: u64,
    pub retained_sessions: u64,
    pub session_ttl_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ResourceUsage {
    pub cpu: u64,
    pub memory_mib: u64,
    pub disk_mib: u64,
}

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
#[cfg_attr(test, derive(Serialize))]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    Ready,
    Stopping,
    Stopped,
    Failed,
    Destroyed,
}

#[derive(Clone, Debug, Deserialize)]
#[cfg_attr(test, derive(Serialize))]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub session_id: String,
    pub state: SessionState,
    pub generation: u64,
    /// Present only when connected to the transitional runtime-selected Core API.
    pub runtime: Option<String>,
    /// Present when Core supports per-session size selection.
    pub size: Option<VmSize>,
    /// Present when Core reports the admitted template's CPU profile.
    pub vcpu_count: Option<u8>,
    /// Present when Core reports the admitted template's memory profile.
    pub memory_mib: Option<u64>,
    pub template_id: String,
    pub core_sha256: String,
    pub volume_id: String,
    pub system_files_volume_id: Option<String>,
    pub workspace_path: String,
    pub guest_ipv4: Ipv4Addr,
    pub ssh_port: u16,
    pub ssh_username: String,
    pub ssh_host_public_key: String,
    pub volume_create_ns: Option<u64>,
    pub storage_attach_ns: Option<u64>,
    pub worker_spawn_ns: Option<u64>,
    pub worker_template_prepare_ns: Option<u64>,
    pub cow_fork_ns: Option<u64>,
    pub vm_create_ns: Option<u64>,
    pub state_restore_ns: Option<u64>,
    pub device_restore_ns: Option<u64>,
    pub vsock_transport_reset_ns: Option<u64>,
    pub vsock_connect_ns: Option<u64>,
    pub vsock_init_ns: Option<u64>,
    pub guest_ready_ns: u64,
    pub system_files_ready_ns: Option<u64>,
    pub storage_ready_ns: u64,
    pub network_ready_ns: u64,
    pub ssh_ready_ns: u64,
    pub access_probe_ns: Option<u64>,
    pub worker_ready_ns: Option<u64>,
    pub session_ready_ns: Option<u64>,
}

#[derive(Deserialize)]
struct ApiHealthResponse {
    runtime: Option<String>,
    sizes: Option<Vec<VmSize>>,
}

#[derive(Serialize)]
struct ApiCreateSessionRequest<'a> {
    session_id: &'a str,
    client_public_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<VmSize>,
}

#[derive(Serialize)]
struct ApiRuntimeCreateSessionRequest<'a> {
    runtime: &'a str,
    client_public_key: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CreateContract {
    CallerAssignedId { sizes: Option<Vec<VmSize>> },
    ServerAssignedId { runtime: String },
}

impl CreateContract {
    pub(crate) fn caller_assigns_id(&self) -> bool {
        matches!(self, Self::CallerAssignedId { .. })
    }

    pub(crate) fn request_size(&self, requested: Option<VmSize>) -> io::Result<Option<VmSize>> {
        let Some(requested) = requested else {
            return Ok(None);
        };
        let Self::CallerAssignedId { sizes } = self else {
            return legacy_size(requested);
        };
        let Some(sizes) = sizes else {
            return legacy_size(requested);
        };
        if sizes.contains(&requested) {
            Ok(Some(requested))
        } else {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!(
                    "the endpoint does not offer the configured {} VM size",
                    requested.label()
                ),
            ))
        }
    }
}

fn legacy_size(requested: VmSize) -> io::Result<Option<VmSize>> {
    if requested == VmSize::Small {
        Ok(None)
    } else {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the endpoint does not support selectable VM sizes; run `jio config` and choose Small",
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiCreateSessionAccepted {
    session_id: String,
    state: SessionState,
}

enum SessionResponse {
    Starting,
    Complete(Box<Session>),
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

    /// Reads the current API key's shared account quota from a hosted control plane.
    pub fn usage(&self) -> io::Result<AccountUsage> {
        let connection = Connection::open(&self.endpoint, &self.api_key, DEFAULT_REQUEST_TIMEOUT)?;
        let response = connection
            .client
            .get(format!("{}/v1/usage", connection.base_url))
            .bearer_auth(&connection.api_key)
            .send()
            .map_err(other)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "jio usage requires a control plane with usage support; standalone Core has no account quotas",
            ));
        }
        let usage: AccountUsage = decode(response)?;
        if usage.account_id.is_empty()
            || usage.account_id.len() > 64
            || !usage
                .account_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
            || usage.limits.cpu == 0
            || usage.limits.memory_mib == 0
            || usage.limits.disk_mib == 0
            || usage.session_ttl_seconds == Some(0)
        {
            return Err(invalid("endpoint returned invalid account usage"));
        }
        Ok(usage)
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

    pub fn create(&self, id: &str, client_public_key: &str) -> io::Result<Session> {
        let contract = self.create_contract()?;
        self.create_with_contract(&contract, id, client_public_key, None)
    }

    /// Creates a session from the requested fixed resource profile.
    pub fn create_with_size(
        &self,
        id: &str,
        client_public_key: &str,
        size: VmSize,
    ) -> io::Result<Session> {
        let contract = self.create_contract()?;
        self.create_with_contract(&contract, id, client_public_key, Some(size))
    }

    pub(crate) fn create_contract(&self) -> io::Result<CreateContract> {
        let connection = Connection::open(&self.endpoint, &self.api_key, DEFAULT_REQUEST_TIMEOUT)?;
        let response = connection
            .client
            .get(format!("{}/v0/health", connection.base_url))
            .bearer_auth(&connection.api_key)
            .send()
            .map_err(other)?;
        let health: ApiHealthResponse = decode(response)?;
        create_contract_from_health(health)
    }

    pub(crate) fn create_with_contract(
        &self,
        contract: &CreateContract,
        id: &str,
        client_public_key: &str,
        size: Option<VmSize>,
    ) -> io::Result<Session> {
        match self.request_create(contract, id, client_public_key, size, false)? {
            SessionResponse::Complete(session) if session.state == SessionState::Ready => {
                Ok(*session)
            }
            SessionResponse::Complete(_) | SessionResponse::Starting => Err(invalid(
                "endpoint did not complete synchronous session creation",
            )),
        }
    }

    pub(crate) fn accept_create_with_contract(
        &self,
        contract: &CreateContract,
        id: &str,
        client_public_key: &str,
        size: Option<VmSize>,
    ) -> io::Result<Option<Session>> {
        match self.request_create(contract, id, client_public_key, size, true)? {
            SessionResponse::Starting => Ok(None),
            SessionResponse::Complete(session) if session.state == SessionState::Ready => {
                Ok(Some(*session))
            }
            SessionResponse::Complete(_) => Err(invalid(
                "endpoint returned an inconsistent asynchronous creation result",
            )),
        }
    }

    fn request_create(
        &self,
        contract: &CreateContract,
        id: &str,
        client_public_key: &str,
        size: Option<VmSize>,
        respond_async: bool,
    ) -> io::Result<SessionResponse> {
        if !valid_session_id(id) {
            return Err(invalid("session ID is invalid"));
        }
        if !valid_ssh_public_key(client_public_key) {
            return Err(invalid(
                "client public key is not one strict ssh-ed25519 key",
            ));
        }
        let asynchronous = respond_async && contract.caller_assigns_id();
        let timeout = if asynchronous {
            DEFAULT_REQUEST_TIMEOUT
        } else {
            CREATE_SESSION_TIMEOUT
        };
        let connection = Connection::open(&self.endpoint, &self.api_key, timeout)?;
        let size = contract.request_size(size)?;
        let body = encode_create_request(contract, id, client_public_key, size)?;
        let mut request = connection
            .client
            .post(format!("{}/v0/sessions", connection.base_url))
            .bearer_auth(&connection.api_key)
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        if asynchronous {
            request = request.header("prefer", "respond-async");
        }
        let response = request.send().map_err(other)?;
        decode_session_response(response, contract.caller_assigns_id().then_some(id), size)
    }

    pub fn get(&self, id: &str) -> io::Result<Session> {
        if !valid_session_id(id) {
            return Err(invalid("session ID is invalid"));
        }
        let connection = Connection::open(&self.endpoint, &self.api_key, DEFAULT_REQUEST_TIMEOUT)?;
        match get_session_response(&connection, id)? {
            SessionResponse::Complete(session) => Ok(*session),
            SessionResponse::Starting => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("session {id} is still starting"),
            )),
        }
    }

    pub(crate) fn wait_until_created(&self, id: &str) -> io::Result<Session> {
        if !valid_session_id(id) {
            return Err(invalid("session ID is invalid"));
        }
        let connection = Connection::open(&self.endpoint, &self.api_key, CREATE_SESSION_TIMEOUT)?;
        let started = Instant::now();
        loop {
            match get_session_response(&connection, id)? {
                SessionResponse::Complete(session) => return Ok(*session),
                SessionResponse::Starting => {}
            }
            if started.elapsed() >= CREATE_SESSION_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("session {id} did not finish creation in time"),
                ));
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    pub fn stop(&self, id: &str) -> io::Result<Session> {
        self.lifecycle(id, "stop", DEFAULT_REQUEST_TIMEOUT, SessionState::Stopped)
    }

    pub fn start(&self, id: &str) -> io::Result<Session> {
        self.lifecycle(id, "start", CREATE_SESSION_TIMEOUT, SessionState::Ready)
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
        if response.status() == StatusCode::NO_CONTENT || response.status() == StatusCode::NOT_FOUND
        {
            Ok(())
        } else {
            Err(response_error(response)?)
        }
    }

    fn lifecycle(
        &self,
        id: &str,
        action: &str,
        timeout: Duration,
        expected: SessionState,
    ) -> io::Result<Session> {
        if !valid_session_id(id) {
            return Err(invalid("session ID is invalid"));
        }
        let connection = Connection::open(&self.endpoint, &self.api_key, timeout)?;
        let generation = self.get(id)?.generation;
        let mut random = [0u8; 16];
        File::open("/dev/urandom")?.read_exact(&mut random)?;
        let response = connection
            .client
            .post(format!("{}/v0/sessions/{id}/{action}", connection.base_url))
            .bearer_auth(&connection.api_key)
            .header("if-match", generation)
            .header("idempotency-key", format!("{:x}", Sha256::digest(random)))
            .send()
            .map_err(other)?;
        let session: Session = decode(response)?;
        validate_session(&session)?;
        if session.session_id != id || session.state != expected {
            return Err(invalid(
                "endpoint returned an inconsistent lifecycle result",
            ));
        }
        Ok(session)
    }
}

fn create_contract_from_health(health: ApiHealthResponse) -> io::Result<CreateContract> {
    match (health.runtime, health.sizes) {
        (Some(runtime), None) if is_runtime(&runtime) => {
            Ok(CreateContract::ServerAssignedId { runtime })
        }
        (Some(_), None) => Err(invalid("endpoint returned an invalid runtime identifier")),
        (Some(_), Some(_)) => Err(invalid(
            "endpoint advertised incompatible runtime and size create contracts",
        )),
        (None, Some(sizes)) if sizes.is_empty() => {
            Err(invalid("endpoint advertised an empty VM size catalog"))
        }
        (None, Some(sizes)) if has_duplicate_sizes(&sizes) => {
            Err(invalid("endpoint advertised duplicate VM sizes"))
        }
        (None, sizes) => Ok(CreateContract::CallerAssignedId { sizes }),
    }
}

fn has_duplicate_sizes(sizes: &[VmSize]) -> bool {
    sizes
        .iter()
        .enumerate()
        .any(|(index, size)| sizes[..index].contains(size))
}

fn encode_create_request(
    contract: &CreateContract,
    id: &str,
    client_public_key: &str,
    size: Option<VmSize>,
) -> io::Result<Vec<u8>> {
    match contract {
        CreateContract::CallerAssignedId { .. } => serde_json::to_vec(&ApiCreateSessionRequest {
            session_id: id,
            client_public_key,
            size,
        }),
        CreateContract::ServerAssignedId { runtime } => {
            if size.is_some() {
                return Err(invalid(
                    "runtime-selected endpoints cannot receive a VM size",
                ));
            }
            serde_json::to_vec(&ApiRuntimeCreateSessionRequest {
                runtime,
                client_public_key,
            })
        }
    }
    .map_err(other)
}

fn get_session_response(connection: &Connection, id: &str) -> io::Result<SessionResponse> {
    let response = connection
        .client
        .get(format!("{}/v0/sessions/{id}", connection.base_url))
        .bearer_auth(&connection.api_key)
        .send()
        .map_err(other)?;
    decode_session_response(response, Some(id), None)
}

fn decode_session_response(
    response: Response,
    expected_id: Option<&str>,
    expected_size: Option<VmSize>,
) -> io::Result<SessionResponse> {
    if response.status() == StatusCode::ACCEPTED {
        let Some(expected_id) = expected_id else {
            return Err(invalid(
                "runtime-selected endpoint returned an asynchronous creation response",
            ));
        };
        let accepted: ApiCreateSessionAccepted = decode(response)?;
        if accepted.session_id != expected_id || accepted.state != SessionState::Starting {
            return Err(invalid(
                "endpoint returned an inconsistent starting session",
            ));
        }
        return Ok(SessionResponse::Starting);
    }
    let session: Session = decode(response)?;
    validate_session(&session)?;
    if expected_id.is_some_and(|id| session.session_id != id) {
        return Err(invalid("endpoint returned a different session ID"));
    }
    if expected_size.is_some_and(|size| session.size != Some(size)) {
        return Err(invalid(
            "endpoint returned a session with a different VM size",
        ));
    }
    Ok(SessionResponse::Complete(Box::new(session)))
}

fn validate_session(session: &Session) -> io::Result<()> {
    if session.runtime.is_some() && session.size.is_some() {
        return Err(invalid(
            "session contains incompatible runtime and size selectors",
        ));
    }
    let system_files_consistent = match (
        &session.system_files_volume_id,
        session.system_files_ready_ns,
    ) {
        (Some(id), Some(ready)) => {
            valid_session_id(id)
                && ready >= session.guest_ready_ns
                && session.storage_ready_ns >= ready
        }
        (None, None) => session.storage_ready_ns >= session.guest_ready_ns,
        _ => false,
    };
    let protocol_consistent = match session.runtime.as_deref() {
        Some(runtime) => {
            is_runtime(runtime)
                && session.system_files_volume_id.is_none()
                && session.system_files_ready_ns.is_none()
                && session.volume_create_ns.is_none()
                && detailed_timings_absent(session)
        }
        None => detailed_timings_consistent(session),
    };
    if !valid_session_id(&session.session_id)
        || session.generation == 0
        || !is_sha256(&session.template_id)
        || !is_sha256(&session.core_sha256)
        || !valid_session_id(&session.volume_id)
        || session.workspace_path != "/workspace"
        || !session.guest_ipv4.is_private()
        || session.ssh_port != 22
        || session.ssh_username != "jio"
        || !valid_ssh_public_key(&session.ssh_host_public_key)
        || session.guest_ready_ns == 0
        || !system_files_consistent
        || !protocol_consistent
        || session.network_ready_ns < session.storage_ready_ns
        || session.ssh_ready_ns < session.network_ready_ns
    {
        return Err(invalid("endpoint returned inconsistent session metadata"));
    }
    Ok(())
}

fn detailed_timings_absent(session: &Session) -> bool {
    session.storage_attach_ns.is_none()
        && session.worker_spawn_ns.is_none()
        && session.worker_template_prepare_ns.is_none()
        && session.cow_fork_ns.is_none()
        && session.vm_create_ns.is_none()
        && session.state_restore_ns.is_none()
        && session.device_restore_ns.is_none()
        && session.vsock_transport_reset_ns.is_none()
        && session.vsock_connect_ns.is_none()
        && session.vsock_init_ns.is_none()
        && session.access_probe_ns.is_none()
        && session.worker_ready_ns.is_none()
        && session.session_ready_ns.is_none()
}

fn detailed_timings_consistent(session: &Session) -> bool {
    let (
        Some(_storage_attach_ns),
        Some(_worker_spawn_ns),
        Some(_worker_template_prepare_ns),
        Some(_cow_fork_ns),
        Some(_vm_create_ns),
        Some(_state_restore_ns),
        Some(_device_restore_ns),
        Some(_vsock_transport_reset_ns),
        Some(_vsock_connect_ns),
        Some(_vsock_init_ns),
        Some(_access_probe_ns),
        Some(worker_ready_ns),
        Some(session_ready_ns),
    ) = (
        session.storage_attach_ns,
        session.worker_spawn_ns,
        session.worker_template_prepare_ns,
        session.cow_fork_ns,
        session.vm_create_ns,
        session.state_restore_ns,
        session.device_restore_ns,
        session.vsock_transport_reset_ns,
        session.vsock_connect_ns,
        session.vsock_init_ns,
        session.access_probe_ns,
        session.worker_ready_ns,
        session.session_ready_ns,
    )
    else {
        return false;
    };
    worker_ready_ns >= session.ssh_ready_ns
        && session_ready_ns >= worker_ready_ns
        && (session.generation == 1) == session.volume_create_ns.is_some()
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

    emit(Event::Phase("Connecting to Jio".into()))?;
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
        let mut builder = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(timeout);
        if let Some(ca) = custom_ca(endpoint)? {
            builder = builder.add_root_certificate(ca);
        }
        let client = builder.build().map_err(other)?;
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

fn custom_ca(endpoint: &str) -> io::Result<Option<reqwest::Certificate>> {
    connection_ca(endpoint, env::var_os("JIO_CA_CERT").as_deref())
}

fn connection_ca(
    endpoint: &str,
    path: Option<&std::ffi::OsStr>,
) -> io::Result<Option<reqwest::Certificate>> {
    let Some(path) = path else {
        // Do not extend private Jio trust to custom destinations.
        return if reqwest::Url::parse(endpoint)
            .is_ok_and(|url| url.as_str().trim_end_matches('/') == DEFAULT_ENDPOINT)
        {
            reqwest::Certificate::from_pem(JIO_CA)
                .map(Some)
                .map_err(other)
        } else {
            Ok(None)
        };
    };
    let mut bytes = Vec::new();
    File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("CA file exceeds limit"));
    }
    reqwest::Certificate::from_pem(&bytes)
        .map(Some)
        .map_err(other)
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
    #[test]
    fn built_in_trust_is_scoped_and_explicit_connections_are_validated() -> std::io::Result<()> {
        use super::{DEFAULT_ENDPOINT, JIO_CA, connection_ca, resolve_endpoint};
        for endpoint in [
            DEFAULT_ENDPOINT.to_owned(),
            format!("{DEFAULT_ENDPOINT}/"),
            format!("{DEFAULT_ENDPOINT}:443"),
        ] {
            assert!(connection_ca(&endpoint, None)?.is_some());
        }
        for endpoint in [
            "https://example.com".to_owned(),
            format!("{DEFAULT_ENDPOINT}:444"),
            format!("{DEFAULT_ENDPOINT}.example.com"),
            "http://127.0.0.1".into(),
        ] {
            assert!(connection_ca(&endpoint, None)?.is_none());
        }
        let pem = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/jio-ca.pem");
        assert!(connection_ca("https://example.com", Some(pem.as_os_str()))?.is_some());
        assert!(connection_ca(DEFAULT_ENDPOINT, Some(pem.join("missing").as_os_str())).is_err());
        assert_eq!(
            resolve_endpoint(Some("http://127.0.0.1:8080/".into()))?,
            "http://127.0.0.1:8080"
        );
        assert!(resolve_endpoint(Some(String::new())).is_err());
        assert!(resolve_endpoint(Some("http://example.com".into())).is_err());
        use sha2::Digest;
        assert_eq!(
            format!("{:x}", sha2::Sha256::digest(JIO_CA)),
            "9d00fd58f0f7dfec8499462faa825e8d06c256f8d36a201e5af15c91dd0d6bf5"
        );
        Ok(())
    }

    #[test]
    fn usage_uses_authenticated_hosted_route_and_rejects_invalid_responses() -> std::io::Result<()>
    {
        use std::io::{BufRead, Write};
        let valid = r#"{"account_id":"demo","limits":{"cpu":8,"memory_mib":16384,"disk_mib":131072},"reserved":{"cpu":4,"memory_mib":8192,"disk_mib":41216},"compute_sessions":1,"retained_sessions":1,"session_ttl_seconds":1800}"#;
        for (status, body, expected) in [
            (200, valid.to_owned(), None),
            (
                200,
                valid.replace("\"cpu\":4", "\"cpu\":-4"),
                Some(std::io::ErrorKind::InvalidData),
            ),
            (
                200,
                valid.replace("demo", "bad\\u001baccount"),
                Some(std::io::ErrorKind::InvalidData),
            ),
            (404, "{}".into(), Some(std::io::ErrorKind::Unsupported)),
            (
                401,
                r#"{"error":"invalid API key"}"#.into(),
                Some(std::io::ErrorKind::Other),
            ),
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
            let endpoint = format!("http://{}", listener.local_addr()?);
            let server = std::thread::spawn(move || -> std::io::Result<()> {
                let (mut stream, _) = listener.accept()?;
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5)))?;
                let mut reader = std::io::BufReader::new(&stream);
                let mut headers = String::new();
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line);
                    assert!(headers.len() < 8192);
                }
                assert!(headers.starts_with("GET /v1/usage HTTP/1.1\r\n"));
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains(&format!("authorization: bearer {}", "a".repeat(64)))
                );
                write!(
                    stream,
                    "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
            });
            let result = super::SessionClient::new(endpoint, "a".repeat(64))?.usage();
            server
                .join()
                .map_err(|_| std::io::Error::other("test server failed"))??;
            if let Some(kind) = expected {
                assert!(matches!(result, Err(error) if error.kind() == kind));
            } else {
                let usage = result?;
                assert_eq!(usage.reserved.cpu, 4);
                assert_eq!(usage.session_ttl_seconds, Some(1800));
            }
        }
        Ok(())
    }

    #[test]
    fn queued_delete_is_not_completed_deletion() -> std::io::Result<()> {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let endpoint = format!("http://{}", listener.local_addr()?);
        let server = std::thread::spawn(move || -> std::io::Result<()> {
            let (mut stream, _) = listener.accept()?;
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes)?;
            stream.write_all(
                b"HTTP/1.1 202 Accepted\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
            )
        });
        let client = super::SessionClient::new(endpoint, "a".repeat(64))?;
        assert!(client.destroy(&"b".repeat(32)).is_err());
        server
            .join()
            .map_err(|_| std::io::Error::other("test server failed"))??;
        Ok(())
    }
    use super::{
        ApiHealthResponse, ApiRunResponse, ApiVmResult, CreateContract, RunRequest, Session,
        SessionClient, SessionState, VmSize, create_contract_from_health, encode_create_request,
        normalize_endpoint, valid_ssh_public_key, validate, validate_session,
    };
    use std::io;
    use std::net::Ipv4Addr;

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
    fn selects_the_create_contract_from_health_metadata() -> io::Result<()> {
        let current: ApiHealthResponse = serde_json::from_str(r#"{"status":"experimental"}"#)?;
        assert_eq!(
            create_contract_from_health(current)?,
            CreateContract::CallerAssignedId { sizes: None }
        );

        let sized: ApiHealthResponse =
            serde_json::from_str(r#"{"status":"experimental","sizes":["small","medium"]}"#)?;
        assert_eq!(
            create_contract_from_health(sized)?,
            CreateContract::CallerAssignedId {
                sizes: Some(vec![VmSize::Small, VmSize::Medium])
            }
        );

        let transitional: ApiHealthResponse =
            serde_json::from_str(r#"{"status":"experimental","runtime":"python3.12-source-v0"}"#)?;
        assert_eq!(
            create_contract_from_health(transitional)?,
            CreateContract::ServerAssignedId {
                runtime: "python3.12-source-v0".into()
            }
        );

        let invalid: ApiHealthResponse = serde_json::from_str(r#"{"runtime":"../python"}"#)?;
        assert!(create_contract_from_health(invalid).is_err());

        for invalid in [
            r#"{"sizes":[]}"#,
            r#"{"sizes":["small","small"]}"#,
            r#"{"runtime":"python3.12-source-v0","sizes":["small"]}"#,
        ] {
            let health: ApiHealthResponse = serde_json::from_str(invalid)?;
            assert!(create_contract_from_health(health).is_err());
        }
        Ok(())
    }

    #[test]
    fn encodes_each_supported_create_contract() -> io::Result<()> {
        let id = "ab".repeat(16);
        let key =
            "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f";
        let current: serde_json::Value = serde_json::from_slice(&encode_create_request(
            &CreateContract::CallerAssignedId { sizes: None },
            &id,
            key,
            None,
        )?)?;
        assert_eq!(current["session_id"], id);
        assert_eq!(current["client_public_key"], key);
        assert!(current.get("runtime").is_none());
        assert!(current.get("size").is_none());
        assert_eq!(current.as_object().map(|object| object.len()), Some(2));

        let sized: serde_json::Value = serde_json::from_slice(&encode_create_request(
            &CreateContract::CallerAssignedId {
                sizes: Some(vec![VmSize::Small, VmSize::Medium]),
            },
            &id,
            key,
            Some(VmSize::Medium),
        )?)?;
        assert_eq!(sized["size"], "medium");
        assert_eq!(sized.as_object().map(|object| object.len()), Some(3));

        let transitional: serde_json::Value = serde_json::from_slice(&encode_create_request(
            &CreateContract::ServerAssignedId {
                runtime: "python3.12-source-v0".into(),
            },
            &id,
            key,
            None,
        )?)?;
        assert_eq!(transitional["runtime"], "python3.12-source-v0");
        assert_eq!(transitional["client_public_key"], key);
        assert!(transitional.get("session_id").is_none());
        Ok(())
    }

    #[test]
    fn gates_size_selection_on_endpoint_capabilities() -> io::Result<()> {
        let legacy = CreateContract::CallerAssignedId { sizes: None };
        assert_eq!(legacy.request_size(Some(VmSize::Small))?, None);
        assert!(legacy.request_size(Some(VmSize::Medium)).is_err());

        let sized = CreateContract::CallerAssignedId {
            sizes: Some(vec![VmSize::Small, VmSize::Medium]),
        };
        assert_eq!(
            sized.request_size(Some(VmSize::Medium))?,
            Some(VmSize::Medium)
        );
        assert!(sized.request_size(Some(VmSize::Large)).is_err());
        Ok(())
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

    #[test]
    fn accepts_core_responses_with_or_without_resource_metadata() -> io::Result<()> {
        let mut wire = serde_json::to_value(persistent_session())?;
        let fields = wire
            .as_object_mut()
            .ok_or_else(|| io::Error::other("session must be an object"))?;
        for field in ["size", "vcpu_count", "memory_mib"] {
            fields.remove(field);
        }
        let legacy: Session = serde_json::from_value(wire.clone())?;
        validate_session(&legacy)?;
        assert_eq!(
            (legacy.size, legacy.vcpu_count, legacy.memory_mib),
            (None, None, None)
        );

        wire["vcpu_count"] = serde_json::json!(2);
        wire["memory_mib"] = serde_json::json!(4096);
        let current: Session = serde_json::from_value(wire.clone())?;
        validate_session(&current)?;
        assert_eq!(
            (current.vcpu_count, current.memory_mib),
            (Some(2), Some(4096))
        );
        wire["unexpected_field"] = serde_json::json!(true);
        assert!(serde_json::from_value::<Session>(wire).is_err());
        Ok(())
    }

    #[test]
    fn validates_the_persistent_session_contract() -> io::Result<()> {
        let session = persistent_session();
        validate_session(&session)?;

        let mut mismatched = session.clone();
        mismatched.system_files_ready_ns = None;
        assert!(validate_session(&mismatched).is_err());

        let mut incompatible = session.clone();
        incompatible.runtime = Some("python3.12-source-v0".into());
        incompatible.size = Some(VmSize::Small);
        assert!(validate_session(&incompatible).is_err());

        let mut restarted = session;
        restarted.generation = 2;
        restarted.volume_create_ns = None;
        validate_session(&restarted)
    }

    #[test]
    fn validates_the_transitional_session_contract_without_inventing_timings() -> io::Result<()> {
        let session: Session = serde_json::from_value(serde_json::json!({
            "session_id": "ab".repeat(16),
            "state": "ready",
            "generation": 1,
            "runtime": "python3.12-source-v0",
            "vcpu_count": 2,
            "memory_mib": 4096,
            "template_id": "c".repeat(64),
            "core_sha256": "d".repeat(64),
            "volume_id": "ef".repeat(16),
            "workspace_path": "/workspace",
            "guest_ipv4": "172.31.10.2",
            "ssh_port": 22,
            "ssh_username": "jio",
            "ssh_host_public_key": "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f",
            "guest_ready_ns": 10,
            "storage_ready_ns": 20,
            "network_ready_ns": 30,
            "ssh_ready_ns": 40
        }))?;
        validate_session(&session)?;
        assert_eq!(session.runtime.as_deref(), Some("python3.12-source-v0"));
        assert_eq!(session.vcpu_count, Some(2));
        assert_eq!(session.memory_mib, Some(4096));
        assert!(session.worker_ready_ns.is_none());

        let mut mixed = session;
        mixed.worker_ready_ns = Some(40);
        assert!(validate_session(&mixed).is_err());
        Ok(())
    }

    fn persistent_session() -> Session {
        Session {
            session_id: "ab".repeat(16),
            state: SessionState::Ready,
            generation: 1,
            runtime: None,
            size: None,
            vcpu_count: Some(2),
            memory_mib: Some(4096),
            template_id: "c".repeat(64),
            core_sha256: "d".repeat(64),
            volume_id: "ef".repeat(16),
            system_files_volume_id: Some("12".repeat(16)),
            workspace_path: "/workspace".into(),
            guest_ipv4: Ipv4Addr::new(172, 31, 10, 2),
            ssh_port: 22,
            ssh_username: "jio".into(),
            ssh_host_public_key:
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAABAgMEBQYHCAkKCwwNDg8QERITFBUWFxgZGhscHR4f"
                    .into(),
            volume_create_ns: Some(1),
            storage_attach_ns: Some(1),
            worker_spawn_ns: Some(1),
            worker_template_prepare_ns: Some(1),
            cow_fork_ns: Some(1),
            vm_create_ns: Some(1),
            state_restore_ns: Some(1),
            device_restore_ns: Some(1),
            vsock_transport_reset_ns: Some(1),
            vsock_connect_ns: Some(2),
            vsock_init_ns: Some(3),
            guest_ready_ns: 10,
            system_files_ready_ns: Some(20),
            storage_ready_ns: 30,
            network_ready_ns: 40,
            ssh_ready_ns: 50,
            access_probe_ns: Some(1),
            worker_ready_ns: Some(60),
            session_ready_ns: Some(70),
        }
    }
}
