use jio_client::{
    CommandResult, MAX_COMMAND_TIMEOUT, Session, SessionState, Vm as CoreVm, VmClient,
};
use napi::bindgen_prelude::{AsyncTask, BigInt, Buffer};
use napi::{Env, Error, Result, Status, Task};
use napi_derive::napi;
use std::env;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECONDS: u32 = 120;

#[napi(object)]
pub struct JioOptions {
    pub endpoint: Option<String>,
    pub api_key: Option<String>,
    pub state_dir: Option<String>,
}

#[napi(object)]
pub struct ExecOptions {
    pub input: Option<Buffer>,
    pub timeout_seconds: Option<u32>,
}

#[napi(object)]
pub struct OperationOptions {
    pub timeout_seconds: Option<u32>,
}

#[napi(object)]
pub struct SessionInfo {
    pub session_id: String,
    #[napi(ts_type = "'starting' | 'ready' | 'stopping' | 'stopped' | 'failed' | 'destroyed'")]
    pub state: String,
    pub generation: BigInt,
    pub runtime: Option<String>,
    pub template_id: String,
    pub core_sha256: String,
    pub volume_id: String,
    pub system_files_volume_id: Option<String>,
    pub workspace_path: String,
    pub guest_ipv4: String,
    pub ssh_port: u16,
    pub ssh_username: String,
    pub ssh_host_public_key: String,
    pub volume_create_ns: Option<BigInt>,
    pub storage_attach_ns: Option<BigInt>,
    pub worker_spawn_ns: Option<BigInt>,
    pub worker_template_prepare_ns: Option<BigInt>,
    pub cow_fork_ns: Option<BigInt>,
    pub vm_create_ns: Option<BigInt>,
    pub state_restore_ns: Option<BigInt>,
    pub device_restore_ns: Option<BigInt>,
    pub vsock_transport_reset_ns: Option<BigInt>,
    pub vsock_connect_ns: Option<BigInt>,
    pub vsock_init_ns: Option<BigInt>,
    pub guest_ready_ns: BigInt,
    pub system_files_ready_ns: Option<BigInt>,
    pub storage_ready_ns: BigInt,
    pub network_ready_ns: BigInt,
    pub ssh_ready_ns: BigInt,
    pub access_probe_ns: Option<BigInt>,
    pub worker_ready_ns: Option<BigInt>,
    pub session_ready_ns: Option<BigInt>,
}

#[napi(object)]
pub struct ExecResult {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: Buffer,
    pub stderr: Buffer,
}

#[napi(js_name = "Jio")]
pub struct JsJio {
    inner: VmClient,
}

#[napi]
impl JsJio {
    #[napi(constructor)]
    pub fn new(options: Option<JioOptions>) -> Result<Self> {
        let options = options.unwrap_or(JioOptions {
            endpoint: None,
            api_key: None,
            state_dir: None,
        });
        let endpoint = configured(options.endpoint, "JIO_ENDPOINT", Some("JIO_HOST"))?;
        let api_key = configured(options.api_key, "JIO_API_KEY", None)?;
        let inner = match options.state_dir {
            Some(directory) => {
                VmClient::with_state_directory(endpoint, api_key, PathBuf::from(directory))
            }
            None => VmClient::new(endpoint, api_key),
        }
        .map_err(to_node_error)?;
        Ok(Self { inner })
    }

    #[napi(getter)]
    pub fn endpoint(&self) -> String {
        self.inner.endpoint().into()
    }

    #[napi(ts_return_type = "Promise<Vm>")]
    pub fn create(&self) -> AsyncTask<CreateTask> {
        AsyncTask::new(CreateTask {
            client: self.inner.clone(),
        })
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn inspect(&self, session_id: String) -> AsyncTask<InspectTask> {
        AsyncTask::new(InspectTask {
            client: self.inner.clone(),
            session_id,
        })
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn get(&self, session_id: String) -> AsyncTask<InspectTask> {
        AsyncTask::new(InspectTask {
            client: self.inner.clone(),
            session_id,
        })
    }

    #[napi(ts_return_type = "Promise<Vm>")]
    pub fn attach(&self, session_id: String) -> AsyncTask<AttachTask> {
        AsyncTask::new(AttachTask {
            client: self.inner.clone(),
            session_id,
        })
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn stop(&self, session_id: String) -> AsyncTask<LifecycleTask> {
        AsyncTask::new(LifecycleTask {
            client: self.inner.clone(),
            session_id,
            action: LifecycleAction::Stop,
        })
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn start(&self, session_id: String) -> AsyncTask<LifecycleTask> {
        AsyncTask::new(LifecycleTask {
            client: self.inner.clone(),
            session_id,
            action: LifecycleAction::Start,
        })
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub fn destroy(&self, session_id: String) -> AsyncTask<DestroyTask> {
        AsyncTask::new(DestroyTask {
            client: self.inner.clone(),
            session_id,
        })
    }
}

#[napi(js_name = "Vm")]
pub struct JsVm {
    inner: CoreVm,
}

#[napi]
impl JsVm {
    #[napi(getter)]
    pub fn id(&self) -> String {
        self.inner.session().session_id.clone()
    }

    #[napi(getter)]
    pub fn session(&self) -> SessionInfo {
        session_info(&self.inner.session())
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn refresh(&self) -> AsyncTask<RefreshTask> {
        AsyncTask::new(RefreshTask {
            vm: self.inner.clone(),
        })
    }

    #[napi(ts_return_type = "Promise<ExecResult>")]
    pub fn exec(
        &self,
        command: String,
        options: Option<ExecOptions>,
    ) -> Result<AsyncTask<ExecTask>> {
        let options = options.unwrap_or(ExecOptions {
            input: None,
            timeout_seconds: None,
        });
        let timeout = timeout_duration(options.timeout_seconds)?;
        Ok(AsyncTask::new(ExecTask {
            vm: self.inner.clone(),
            command,
            input: options.input.map(|input| input.to_vec()),
            timeout,
        }))
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub fn write_file(
        &self,
        remote_path: String,
        contents: Buffer,
        options: Option<OperationOptions>,
    ) -> Result<AsyncTask<WriteFileTask>> {
        let timeout = timeout_duration(options.and_then(|value| value.timeout_seconds))?;
        Ok(AsyncTask::new(WriteFileTask {
            vm: self.inner.clone(),
            remote_path,
            contents: contents.to_vec(),
            timeout,
        }))
    }

    #[napi(ts_return_type = "Promise<Buffer>")]
    pub fn read_file(
        &self,
        remote_path: String,
        options: Option<OperationOptions>,
    ) -> Result<AsyncTask<ReadFileTask>> {
        let timeout = timeout_duration(options.and_then(|value| value.timeout_seconds))?;
        Ok(AsyncTask::new(ReadFileTask {
            vm: self.inner.clone(),
            remote_path,
            timeout,
        }))
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn stop(&self) -> AsyncTask<VmLifecycleTask> {
        AsyncTask::new(VmLifecycleTask {
            vm: self.inner.clone(),
            action: LifecycleAction::Stop,
        })
    }

    #[napi(ts_return_type = "Promise<SessionInfo>")]
    pub fn start(&self) -> AsyncTask<VmLifecycleTask> {
        AsyncTask::new(VmLifecycleTask {
            vm: self.inner.clone(),
            action: LifecycleAction::Start,
        })
    }

    #[napi(ts_return_type = "Promise<void>")]
    pub fn destroy(&self) -> AsyncTask<DestroyVmTask> {
        AsyncTask::new(DestroyVmTask {
            vm: self.inner.clone(),
        })
    }
}

pub struct CreateTask {
    client: VmClient,
}

impl Task for CreateTask {
    type Output = CoreVm;
    type JsValue = JsVm;

    fn compute(&mut self) -> Result<Self::Output> {
        self.client.create().map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, inner: Self::Output) -> Result<Self::JsValue> {
        Ok(JsVm { inner })
    }
}

#[derive(Clone, Copy)]
enum LifecycleAction {
    Stop,
    Start,
}

pub struct LifecycleTask {
    client: VmClient,
    session_id: String,
    action: LifecycleAction,
}

impl Task for LifecycleTask {
    type Output = Session;
    type JsValue = SessionInfo;

    fn compute(&mut self) -> Result<Self::Output> {
        match self.action {
            LifecycleAction::Stop => self.client.stop(&self.session_id),
            LifecycleAction::Start => self.client.start(&self.session_id),
        }
        .map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, session: Self::Output) -> Result<Self::JsValue> {
        Ok(session_info(&session))
    }
}

pub struct VmLifecycleTask {
    vm: CoreVm,
    action: LifecycleAction,
}

impl Task for VmLifecycleTask {
    type Output = Session;
    type JsValue = SessionInfo;

    fn compute(&mut self) -> Result<Self::Output> {
        match self.action {
            LifecycleAction::Stop => self.vm.stop(),
            LifecycleAction::Start => self.vm.start(),
        }
        .map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, session: Self::Output) -> Result<Self::JsValue> {
        Ok(session_info(&session))
    }
}

pub struct InspectTask {
    client: VmClient,
    session_id: String,
}

impl Task for InspectTask {
    type Output = Session;
    type JsValue = SessionInfo;

    fn compute(&mut self) -> Result<Self::Output> {
        self.client.inspect(&self.session_id).map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, session: Self::Output) -> Result<Self::JsValue> {
        Ok(session_info(&session))
    }
}

pub struct AttachTask {
    client: VmClient,
    session_id: String,
}

impl Task for AttachTask {
    type Output = CoreVm;
    type JsValue = JsVm;

    fn compute(&mut self) -> Result<Self::Output> {
        self.client.attach(&self.session_id).map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, inner: Self::Output) -> Result<Self::JsValue> {
        Ok(JsVm { inner })
    }
}

pub struct DestroyTask {
    client: VmClient,
    session_id: String,
}

impl Task for DestroyTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.client.destroy(&self.session_id).map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct RefreshTask {
    vm: CoreVm,
}

impl Task for RefreshTask {
    type Output = Session;
    type JsValue = SessionInfo;

    fn compute(&mut self) -> Result<Self::Output> {
        self.vm.refresh().map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, session: Self::Output) -> Result<Self::JsValue> {
        Ok(session_info(&session))
    }
}

pub struct ExecTask {
    vm: CoreVm,
    command: String,
    input: Option<Vec<u8>>,
    timeout: Duration,
}

impl Task for ExecTask {
    type Output = CommandResult;
    type JsValue = ExecResult;

    fn compute(&mut self) -> Result<Self::Output> {
        self.vm
            .exec_with_input(&self.command, self.input.as_deref(), self.timeout)
            .map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, result: Self::Output) -> Result<Self::JsValue> {
        Ok(ExecResult {
            exit_code: result.exit_code,
            success: result.success(),
            stdout: Buffer::from(result.stdout),
            stderr: Buffer::from(result.stderr),
        })
    }
}

pub struct WriteFileTask {
    vm: CoreVm,
    remote_path: String,
    contents: Vec<u8>,
    timeout: Duration,
}

impl Task for WriteFileTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.vm
            .write_file(&self.remote_path, &self.contents, self.timeout)
            .map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

pub struct ReadFileTask {
    vm: CoreVm,
    remote_path: String,
    timeout: Duration,
}

impl Task for ReadFileTask {
    type Output = Vec<u8>;
    type JsValue = Buffer;

    fn compute(&mut self) -> Result<Self::Output> {
        self.vm
            .read_file(&self.remote_path, self.timeout)
            .map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, contents: Self::Output) -> Result<Self::JsValue> {
        Ok(Buffer::from(contents))
    }
}

pub struct DestroyVmTask {
    vm: CoreVm,
}

impl Task for DestroyVmTask {
    type Output = ();
    type JsValue = ();

    fn compute(&mut self) -> Result<Self::Output> {
        self.vm.destroy().map_err(to_node_error)
    }

    fn resolve(&mut self, _env: Env, output: Self::Output) -> Result<Self::JsValue> {
        Ok(output)
    }
}

fn session_info(session: &Session) -> SessionInfo {
    SessionInfo {
        session_id: session.session_id.clone(),
        state: state_name(session.state).into(),
        generation: BigInt::from(session.generation),
        runtime: session.runtime.clone(),
        template_id: session.template_id.clone(),
        core_sha256: session.core_sha256.clone(),
        volume_id: session.volume_id.clone(),
        system_files_volume_id: session.system_files_volume_id.clone(),
        workspace_path: session.workspace_path.clone(),
        guest_ipv4: session.guest_ipv4.to_string(),
        ssh_port: session.ssh_port,
        ssh_username: session.ssh_username.clone(),
        ssh_host_public_key: session.ssh_host_public_key.clone(),
        volume_create_ns: session.volume_create_ns.map(BigInt::from),
        storage_attach_ns: session.storage_attach_ns.map(BigInt::from),
        worker_spawn_ns: session.worker_spawn_ns.map(BigInt::from),
        worker_template_prepare_ns: session.worker_template_prepare_ns.map(BigInt::from),
        cow_fork_ns: session.cow_fork_ns.map(BigInt::from),
        vm_create_ns: session.vm_create_ns.map(BigInt::from),
        state_restore_ns: session.state_restore_ns.map(BigInt::from),
        device_restore_ns: session.device_restore_ns.map(BigInt::from),
        vsock_transport_reset_ns: session.vsock_transport_reset_ns.map(BigInt::from),
        vsock_connect_ns: session.vsock_connect_ns.map(BigInt::from),
        vsock_init_ns: session.vsock_init_ns.map(BigInt::from),
        guest_ready_ns: BigInt::from(session.guest_ready_ns),
        system_files_ready_ns: session.system_files_ready_ns.map(BigInt::from),
        storage_ready_ns: BigInt::from(session.storage_ready_ns),
        network_ready_ns: BigInt::from(session.network_ready_ns),
        ssh_ready_ns: BigInt::from(session.ssh_ready_ns),
        access_probe_ns: session.access_probe_ns.map(BigInt::from),
        worker_ready_ns: session.worker_ready_ns.map(BigInt::from),
        session_ready_ns: session.session_ready_ns.map(BigInt::from),
    }
}

fn state_name(state: SessionState) -> &'static str {
    match state {
        SessionState::Starting => "starting",
        SessionState::Ready => "ready",
        SessionState::Stopping => "stopping",
        SessionState::Stopped => "stopped",
        SessionState::Failed => "failed",
        SessionState::Destroyed => "destroyed",
    }
}

fn configured(
    explicit: Option<String>,
    variable: &str,
    legacy_variable: Option<&str>,
) -> Result<String> {
    if let Some(value) = explicit {
        return Ok(value);
    }
    match env::var(variable) {
        Ok(value) => return Ok(value),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(Error::new(
                Status::InvalidArg,
                format!("{variable} is not valid UTF-8"),
            ));
        }
        Err(env::VarError::NotPresent) => {}
    }
    if let Some(legacy_variable) = legacy_variable {
        match env::var(legacy_variable) {
            Ok(value) => return Ok(value),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(Error::new(
                    Status::InvalidArg,
                    format!("{legacy_variable} is not valid UTF-8"),
                ));
            }
            Err(env::VarError::NotPresent) => {}
        }
    }
    Err(Error::new(
        Status::InvalidArg,
        format!("{variable} is required"),
    ))
}

fn timeout_duration(seconds: Option<u32>) -> Result<Duration> {
    let timeout = Duration::from_secs(u64::from(seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS)));
    if timeout.is_zero() || timeout > MAX_COMMAND_TIMEOUT {
        return Err(Error::new(
            Status::InvalidArg,
            format!(
                "timeoutSeconds must be between 1 and {}",
                MAX_COMMAND_TIMEOUT.as_secs()
            ),
        ));
    }
    Ok(timeout)
}

fn to_node_error(error: io::Error) -> Error {
    let status = match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => Status::InvalidArg,
        _ => Status::GenericFailure,
    };
    Error::new(status, error.to_string())
}
