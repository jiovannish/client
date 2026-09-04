use jio_client::{
    CommandResult, DEFAULT_COMMAND_TIMEOUT, MAX_COMMAND_TIMEOUT, Session, SessionState, Vm,
    VmClient,
};
use pyo3::create_exception;
use pyo3::exceptions::{PyException, PyFileNotFoundError, PyTimeoutError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::env;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

create_exception!(_native, JioError, PyException);

#[pyclass(name = "Jio", frozen)]
struct PyJio {
    inner: VmClient,
}

#[pymethods]
impl PyJio {
    #[new]
    #[pyo3(signature = (endpoint=None, api_key=None, state_dir=None))]
    fn new(
        endpoint: Option<String>,
        api_key: Option<String>,
        state_dir: Option<String>,
    ) -> PyResult<Self> {
        let endpoint = configured(endpoint, "JIO_ENDPOINT", Some("JIO_HOST"))?;
        let api_key = configured(api_key, "JIO_API_KEY", None)?;
        let inner = match state_dir {
            Some(directory) => {
                VmClient::with_state_directory(endpoint, api_key, PathBuf::from(directory))
            }
            None => VmClient::new(endpoint, api_key),
        }
        .map_err(to_python_error)?;
        Ok(Self { inner })
    }

    #[getter]
    fn endpoint(&self) -> &str {
        self.inner.endpoint()
    }

    fn create(&self, py: Python<'_>) -> PyResult<PyVm> {
        let client = self.inner.clone();
        let created = py
            .detach(move || client.create())
            .map_err(to_python_error)?;
        Ok(PyVm { inner: created })
    }

    fn inspect(&self, py: Python<'_>, session_id: String) -> PyResult<PySession> {
        let client = self.inner.clone();
        py.detach(move || client.inspect(&session_id))
            .map(|session| PySession::from(&session))
            .map_err(to_python_error)
    }

    fn get(&self, py: Python<'_>, session_id: String) -> PyResult<PySession> {
        self.inspect(py, session_id)
    }

    fn attach(&self, py: Python<'_>, session_id: String) -> PyResult<PyVm> {
        let client = self.inner.clone();
        py.detach(move || client.attach(&session_id))
            .map(|inner| PyVm { inner })
            .map_err(to_python_error)
    }

    fn stop(&self, py: Python<'_>, session_id: String) -> PyResult<PySession> {
        let client = self.inner.clone();
        py.detach(move || client.stop(&session_id))
            .map(|session| PySession::from(&session))
            .map_err(to_python_error)
    }

    fn start(&self, py: Python<'_>, session_id: String) -> PyResult<PySession> {
        let client = self.inner.clone();
        py.detach(move || client.start(&session_id))
            .map(|session| PySession::from(&session))
            .map_err(to_python_error)
    }

    fn destroy(&self, py: Python<'_>, session_id: String) -> PyResult<()> {
        let client = self.inner.clone();
        py.detach(move || client.destroy(&session_id))
            .map_err(to_python_error)
    }

    fn __repr__(&self) -> String {
        format!("Jio(endpoint={:?})", self.inner.endpoint())
    }
}

#[pyclass(name = "Vm", frozen)]
struct PyVm {
    inner: Vm,
}

#[pymethods]
impl PyVm {
    #[getter]
    fn id(&self) -> String {
        self.inner.session().session_id
    }

    #[getter]
    fn session(&self) -> PySession {
        PySession::from(&self.inner.session())
    }

    fn refresh(&self, py: Python<'_>) -> PyResult<PySession> {
        let vm = self.inner.clone();
        py.detach(move || vm.refresh())
            .map(|session| PySession::from(&session))
            .map_err(to_python_error)
    }

    #[pyo3(signature = (command, *, input=None, timeout=120))]
    fn exec(
        &self,
        py: Python<'_>,
        command: String,
        input: Option<&Bound<'_, PyBytes>>,
        timeout: u64,
    ) -> PyResult<PyCommandResult> {
        let input = input.map(|value| value.as_bytes().to_vec());
        let timeout = timeout_duration(timeout)?;
        let vm = self.inner.clone();
        py.detach(move || vm.exec_with_input(&command, input.as_deref(), timeout))
            .map(PyCommandResult::from)
            .map_err(to_python_error)
    }

    #[pyo3(signature = (remote_path, contents, *, timeout=120))]
    fn write_file(
        &self,
        py: Python<'_>,
        remote_path: String,
        contents: &Bound<'_, PyBytes>,
        timeout: u64,
    ) -> PyResult<()> {
        let contents = contents.as_bytes().to_vec();
        let timeout = timeout_duration(timeout)?;
        let vm = self.inner.clone();
        py.detach(move || vm.write_file(&remote_path, &contents, timeout))
            .map_err(to_python_error)
    }

    #[pyo3(signature = (remote_path, *, timeout=120))]
    fn read_file<'py>(
        &self,
        py: Python<'py>,
        remote_path: String,
        timeout: u64,
    ) -> PyResult<Bound<'py, PyBytes>> {
        let timeout = timeout_duration(timeout)?;
        let vm = self.inner.clone();
        let contents = py
            .detach(move || vm.read_file(&remote_path, timeout))
            .map_err(to_python_error)?;
        Ok(PyBytes::new(py, &contents))
    }

    fn stop(&self, py: Python<'_>) -> PyResult<PySession> {
        let vm = self.inner.clone();
        py.detach(move || vm.stop())
            .map(|session| PySession::from(&session))
            .map_err(to_python_error)
    }

    fn start(&self, py: Python<'_>) -> PyResult<PySession> {
        let vm = self.inner.clone();
        py.detach(move || vm.start())
            .map(|session| PySession::from(&session))
            .map_err(to_python_error)
    }

    fn destroy(&self, py: Python<'_>) -> PyResult<()> {
        let vm = self.inner.clone();
        py.detach(move || vm.destroy()).map_err(to_python_error)
    }

    fn __repr__(&self) -> String {
        format!("Vm(id={:?})", self.inner.session().session_id)
    }
}

#[pyclass(name = "Session", frozen, get_all, skip_from_py_object)]
#[derive(Clone)]
struct PySession {
    session_id: String,
    state: String,
    generation: u64,
    runtime: Option<String>,
    template_id: String,
    core_sha256: String,
    volume_id: String,
    system_files_volume_id: Option<String>,
    workspace_path: String,
    guest_ipv4: String,
    ssh_port: u16,
    ssh_username: String,
    ssh_host_public_key: String,
    volume_create_ns: Option<u64>,
    storage_attach_ns: Option<u64>,
    worker_spawn_ns: Option<u64>,
    worker_template_prepare_ns: Option<u64>,
    cow_fork_ns: Option<u64>,
    vm_create_ns: Option<u64>,
    state_restore_ns: Option<u64>,
    device_restore_ns: Option<u64>,
    vsock_transport_reset_ns: Option<u64>,
    vsock_connect_ns: Option<u64>,
    vsock_init_ns: Option<u64>,
    guest_ready_ns: u64,
    system_files_ready_ns: Option<u64>,
    storage_ready_ns: u64,
    network_ready_ns: u64,
    ssh_ready_ns: u64,
    access_probe_ns: Option<u64>,
    worker_ready_ns: Option<u64>,
    session_ready_ns: Option<u64>,
}

impl From<&Session> for PySession {
    fn from(session: &Session) -> Self {
        Self {
            session_id: session.session_id.clone(),
            state: state_name(session.state).into(),
            generation: session.generation,
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
            volume_create_ns: session.volume_create_ns,
            storage_attach_ns: session.storage_attach_ns,
            worker_spawn_ns: session.worker_spawn_ns,
            worker_template_prepare_ns: session.worker_template_prepare_ns,
            cow_fork_ns: session.cow_fork_ns,
            vm_create_ns: session.vm_create_ns,
            state_restore_ns: session.state_restore_ns,
            device_restore_ns: session.device_restore_ns,
            vsock_transport_reset_ns: session.vsock_transport_reset_ns,
            vsock_connect_ns: session.vsock_connect_ns,
            vsock_init_ns: session.vsock_init_ns,
            guest_ready_ns: session.guest_ready_ns,
            system_files_ready_ns: session.system_files_ready_ns,
            storage_ready_ns: session.storage_ready_ns,
            network_ready_ns: session.network_ready_ns,
            ssh_ready_ns: session.ssh_ready_ns,
            access_probe_ns: session.access_probe_ns,
            worker_ready_ns: session.worker_ready_ns,
            session_ready_ns: session.session_ready_ns,
        }
    }
}

#[pymethods]
impl PySession {
    fn __repr__(&self) -> String {
        format!(
            "Session(session_id={:?}, state={:?}, generation={})",
            self.session_id, self.state, self.generation
        )
    }
}

#[pyclass(name = "CommandResult", frozen)]
struct PyCommandResult {
    exit_code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl From<CommandResult> for PyCommandResult {
    fn from(result: CommandResult) -> Self {
        Self {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        }
    }
}

#[pymethods]
impl PyCommandResult {
    #[getter]
    fn exit_code(&self) -> i32 {
        self.exit_code
    }

    #[getter]
    fn success(&self) -> bool {
        self.exit_code == 0
    }

    #[getter]
    fn stdout<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.stdout)
    }

    #[getter]
    fn stderr<'py>(&self, py: Python<'py>) -> Bound<'py, PyBytes> {
        PyBytes::new(py, &self.stderr)
    }

    #[getter]
    fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    #[getter]
    fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    fn raise_for_status(&self) -> PyResult<()> {
        if self.exit_code == 0 {
            return Ok(());
        }
        Err(JioError::new_err(format!(
            "remote command exited with {}: {}",
            self.exit_code,
            String::from_utf8_lossy(&self.stderr).trim()
        )))
    }

    fn __repr__(&self) -> String {
        format!(
            "CommandResult(exit_code={}, stdout_bytes={}, stderr_bytes={})",
            self.exit_code,
            self.stdout.len(),
            self.stderr.len()
        )
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
) -> PyResult<String> {
    if let Some(value) = explicit {
        return Ok(value);
    }
    match env::var(variable) {
        Ok(value) => return Ok(value),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(PyValueError::new_err(format!(
                "{variable} is not valid UTF-8"
            )));
        }
        Err(env::VarError::NotPresent) => {}
    }
    if let Some(legacy_variable) = legacy_variable {
        match env::var(legacy_variable) {
            Ok(value) => return Ok(value),
            Err(env::VarError::NotUnicode(_)) => {
                return Err(PyValueError::new_err(format!(
                    "{legacy_variable} is not valid UTF-8"
                )));
            }
            Err(env::VarError::NotPresent) => {}
        }
    }
    Err(PyValueError::new_err(format!("{variable} is required")))
}

fn timeout_duration(seconds: u64) -> PyResult<Duration> {
    let timeout = Duration::from_secs(seconds);
    if timeout.is_zero() || timeout > MAX_COMMAND_TIMEOUT {
        return Err(PyValueError::new_err(format!(
            "timeout must be between 1 and {} seconds",
            MAX_COMMAND_TIMEOUT.as_secs()
        )));
    }
    Ok(timeout)
}

fn to_python_error(error: io::Error) -> PyErr {
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::InvalidInput => {
            PyValueError::new_err(error.to_string())
        }
        io::ErrorKind::NotFound => PyFileNotFoundError::new_err(error.to_string()),
        io::ErrorKind::TimedOut => PyTimeoutError::new_err(error.to_string()),
        _ => JioError::new_err(error.to_string()),
    }
}

#[pymodule]
fn _native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_class::<PyJio>()?;
    module.add_class::<PyVm>()?;
    module.add_class::<PySession>()?;
    module.add_class::<PyCommandResult>()?;
    module.add("JioError", module.py().get_type::<JioError>())?;
    module.add("DEFAULT_COMMAND_TIMEOUT", DEFAULT_COMMAND_TIMEOUT.as_secs())?;
    Ok(())
}
