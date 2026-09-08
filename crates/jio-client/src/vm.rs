use crate::{
    CreateContract, Session, SessionClient, SessionState, VmSize, valid_session_id,
    valid_ssh_public_key,
};
use std::env;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
pub const MAX_COMMAND_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub const MAX_FILE_BYTES: usize = 16 * 1024 * 1024;

const MAX_COMMAND_BYTES: usize = 64 * 1024;
const MAX_COMMAND_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_REMOTE_PATH_BYTES: usize = 4096;
const MAX_LOCAL_TEXT_BYTES: usize = 1024;
const PRIVATE_KEY: &str = "id_ed25519";
const PUBLIC_KEY: &str = "id_ed25519.pub";
const KNOWN_HOSTS: &str = "known_hosts";
const REQUESTED_SIZE: &str = "requested-size";
const CURRENT_SESSION: &str = "current-session";
const CONNECTION_PREPARE_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECTION_PREPARE_RETRY: Duration = Duration::from_millis(25);
const CONNECTION_READY_MARKER: &[u8] = b"\x1dJIO_SSH_READY_V1\x1d";
const CONNECTION_READY_COMMAND: &str =
    "printf '\\035JIO_SSH_READY_V1\\035'; exec \"${SHELL:-/bin/sh}\" -l";
const MAX_CONNECTION_PRELUDE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy)]
enum TtyMode {
    Disabled,
    Automatic,
    Forced,
}

/// A developer-facing client for creating and using retained Jio VMs.
///
/// `VmClient` owns local SSH credentials and pins the guest host key returned by
/// Core. It uses the same state directory as the CLI, so a VM created from an SDK
/// can later be attached with `jio connect` on the same machine.
///
/// ```no_run
/// use jio_client::{DEFAULT_COMMAND_TIMEOUT, VmClient};
///
/// # fn main() -> std::io::Result<()> {
/// let client = VmClient::new(
///     "ubuntu@jio-host",
///     "0123456789abcdef0123456789abcdef",
/// )?;
/// let vm = client.create()?;
/// let result = vm.exec("printf '42\\n'", DEFAULT_COMMAND_TIMEOUT)?;
/// assert!(result.success());
/// vm.destroy()?;
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct VmClient {
    api: SessionClient,
    sessions: PathBuf,
}

impl VmClient {
    pub fn new(endpoint: impl Into<String>, api_key: impl Into<String>) -> io::Result<Self> {
        let root = match env::var_os("JIO_STATE_DIR") {
            Some(path) => PathBuf::from(path),
            None => env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| invalid_input("HOME or JIO_STATE_DIR is required"))?
                .join(".jio"),
        };
        Self::with_state_directory(endpoint, api_key, root)
    }

    pub fn with_state_directory(
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
        root: impl Into<PathBuf>,
    ) -> io::Result<Self> {
        let api = SessionClient::new(endpoint, api_key)?;
        let root = root.into();
        create_private_directory(&root)?;
        let sessions = root.join("sessions");
        create_private_directory(&sessions)?;
        Ok(Self { api, sessions })
    }

    pub fn endpoint(&self) -> &str {
        self.api.endpoint()
    }

    /// Returns the session selected by the most recent successful CLI create or connect.
    pub fn current_session_id(&self) -> io::Result<Option<String>> {
        let state = self.state_directory()?;
        require_directory(state)?;
        let path = state.join(CURRENT_SESSION);
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        require_private_file(&path)?;
        let id = String::from_utf8(read_bounded(&path, 32)?).map_err(invalid_data)?;
        if !valid_session_id(&id) {
            return Err(invalid_data("local current session ID is invalid"));
        }
        Ok(Some(id))
    }

    /// Selects a session as the default target for later CLI commands.
    pub fn set_current_session_id(&self, id: &str) -> io::Result<()> {
        if !valid_session_id(id) {
            return Err(invalid_input("session ID is invalid"));
        }
        replace_private_file(self.state_directory()?, CURRENT_SESSION, id.as_bytes())
    }

    /// Clears the default target if it still names `id`.
    pub fn clear_current_session_id(&self, id: &str) -> io::Result<()> {
        if !valid_session_id(id) {
            return Err(invalid_input("session ID is invalid"));
        }
        if self.current_session_id()?.as_deref() != Some(id) {
            return Ok(());
        }
        let state = self.state_directory()?;
        fs::remove_file(state.join(CURRENT_SESSION))?;
        File::open(state)?.sync_all()
    }

    fn state_directory(&self) -> io::Result<&Path> {
        self.sessions
            .parent()
            .ok_or_else(|| invalid_data("local session state directory has no parent"))
    }

    pub fn create(&self) -> io::Result<Vm> {
        self.create_and_report_id(|_| {})
    }

    /// Creates a VM from the requested fixed resource profile.
    pub fn create_with_size(&self, size: VmSize) -> io::Result<Vm> {
        self.create_with_size_and_report_id(size, |_| {})
    }

    /// Creates a VM and reports its ID as soon as the endpoint contract permits.
    ///
    /// Current endpoints accept a caller-reserved ID, which is reported before
    /// remote creation begins. Transitional endpoints assign the ID themselves,
    /// so it is reported only after their synchronous create response arrives.
    pub fn create_and_report_id(&self, report: impl FnOnce(&str)) -> io::Result<Vm> {
        self.create_and_report_optional_size(None, report)
    }

    /// Creates a sized VM and reports its ID as soon as the endpoint contract permits.
    pub fn create_with_size_and_report_id(
        &self,
        size: VmSize,
        report: impl FnOnce(&str),
    ) -> io::Result<Vm> {
        self.create_and_report_optional_size(Some(size), report)
    }

    fn create_and_report_optional_size(
        &self,
        size: Option<VmSize>,
        report: impl FnOnce(&str),
    ) -> io::Result<Vm> {
        let contract = self.api.create_contract()?;
        let size = contract.request_size(size)?;
        self.create_with_contract_and_report(&contract, size, report)
    }

    fn create_with_contract_and_report(
        &self,
        contract: &CreateContract,
        size: Option<VmSize>,
        report: impl FnOnce(&str),
    ) -> io::Result<Vm> {
        let id = random_session_id()?;
        let target = self.sessions.join(&id);
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "generated session ID already exists locally",
            ));
        }
        let mut report = Some(report);
        if contract.caller_assigns_id() {
            if let Some(report) = report.take() {
                report(&id);
            }
        }
        let temporary = temporary_directory(&self.sessions)?;
        let result = self.create_inner(contract, size, &id, &temporary);
        let result = if contract.caller_assigns_id() {
            result.map_err(|error| self.retain_failed_create(&id, &temporary, error))
        } else {
            result
        };
        if result.is_err() && !contract.caller_assigns_id() {
            let _ = remove_session_files(&temporary);
        }
        let vm = result?;
        if let Some(report) = report {
            report(&vm.id());
        }
        Ok(vm)
    }

    /// Starts creating a VM and returns once Core owns the in-progress session.
    ///
    /// The local SSH authority is retained immediately so a later `attach` can
    /// wait for readiness and pin the guest host key returned by Core.
    pub fn accept_create_and_report_id(&self, report: impl FnOnce(&str)) -> io::Result<String> {
        self.accept_create_with_optional_size(None, report)
    }

    /// Starts creating a sized VM and returns once Core owns the in-progress session.
    pub fn accept_create_with_size_and_report_id(
        &self,
        size: VmSize,
        report: impl FnOnce(&str),
    ) -> io::Result<String> {
        self.accept_create_with_optional_size(Some(size), report)
    }

    fn accept_create_with_optional_size(
        &self,
        size: Option<VmSize>,
        report: impl FnOnce(&str),
    ) -> io::Result<String> {
        let contract = self.api.create_contract()?;
        let size = contract.request_size(size)?;
        if !contract.caller_assigns_id() {
            return self
                .create_with_contract_and_report(&contract, size, report)
                .map(|vm| vm.id());
        }
        let id = random_session_id()?;
        let target = self.sessions.join(&id);
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "generated session ID already exists locally",
            ));
        }
        report(&id);
        let temporary = temporary_directory(&self.sessions)?;
        self.accept_create_inner(&contract, size, &id, &temporary)
            .map_err(|error| self.retain_failed_create(&id, &temporary, error))
    }

    fn retain_failed_create(&self, id: &str, temporary: &Path, error: io::Error) -> io::Error {
        // A lost/rejected response is not proof that Core never accepted create.
        // Keep authority under the caller-reserved ID, including for synchronous SDK callers.
        let saved = (|| -> io::Result<()> {
            if temporary.try_exists()? {
                require_directory(temporary)?;
                let target = self.session_directory(id)?;
                if target.try_exists()? {
                    return Err(io::Error::other("local target already exists"));
                }
                fs::rename(temporary, target)?;
                File::open(&self.sessions)?.sync_all()?;
            }
            Ok(())
        })();
        match saved {
            Ok(()) => io::Error::new(
                error.kind(),
                format!(
                    "create {id}: {error}; local state retained; inspect or destroy this ID before retrying"
                ),
            ),
            Err(save) => io::Error::new(
                error.kind(),
                format!(
                    "create {id}: {error}; recover local authority at {} ({save})",
                    temporary.display()
                ),
            ),
        }
    }

    fn create_inner(
        &self,
        contract: &CreateContract,
        size: Option<VmSize>,
        id: &str,
        temporary: &Path,
    ) -> io::Result<Vm> {
        let public_key = generate_client_authority(temporary)?;
        persist_requested_size(temporary, size)?;
        let public_path = temporary.join(PUBLIC_KEY);
        let session = self
            .api
            .create_with_contract(contract, id, &public_key, size)?;
        let finalize = (|| {
            write_known_hosts(
                &temporary.join(KNOWN_HOSTS),
                &session.session_id,
                &session.ssh_host_public_key,
            )?;
            fs::remove_file(public_path)?;
            let target = self.sessions.join(&session.session_id);
            if target.try_exists()? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "local session directory already exists",
                ));
            }
            fs::rename(temporary, target)
        })();
        if let Err(error) = finalize {
            let _ = self.api.destroy(&session.session_id);
            return Err(error);
        }
        Ok(Vm {
            owner: self.clone(),
            session: Arc::new(RwLock::new(session)),
        })
    }

    fn accept_create_inner(
        &self,
        contract: &CreateContract,
        size: Option<VmSize>,
        id: &str,
        temporary: &Path,
    ) -> io::Result<String> {
        let public_key = generate_client_authority(temporary)?;
        persist_requested_size(temporary, size)?;
        let ready = self
            .api
            .accept_create_with_contract(contract, id, &public_key, size)?;
        let finalize = (|| {
            if let Some(session) = ready {
                write_known_hosts(
                    &temporary.join(KNOWN_HOSTS),
                    &session.session_id,
                    &session.ssh_host_public_key,
                )?;
                fs::remove_file(temporary.join(PUBLIC_KEY))?;
            }
            let target = self.sessions.join(id);
            if target.try_exists()? {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "local session directory already exists",
                ));
            }
            fs::rename(temporary, &target)?;
            File::open(&self.sessions)?.sync_all()
        })();
        if let Err(failure) = finalize {
            // Once Core acknowledges the request it owns the VM lifecycle. If
            // local authority persistence fails, wait for a terminal creation
            // result before asking Core to remove that otherwise-orphaned VM.
            let _ = self.api.wait_until_created(id);
            let _ = self.api.destroy(id);
            return Err(failure);
        }
        Ok(id.to_owned())
    }

    /// Returns lifecycle metadata without requiring this machine to own the SSH key.
    pub fn inspect(&self, id: &str) -> io::Result<Session> {
        self.api.get(id)
    }

    /// Attaches a programmable handle using credentials previously created locally.
    pub fn attach(&self, id: &str) -> io::Result<Vm> {
        let session = self.api.wait_until_created(id)?;
        self.credentials(&session)?;
        Ok(Vm {
            owner: self.clone(),
            session: Arc::new(RwLock::new(session)),
        })
    }

    pub fn stop(&self, id: &str) -> io::Result<Session> {
        let current = self.api.get(id)?;
        self.credentials(&current)?;
        let stopped = self.api.stop(id)?;
        if !same_session_identity(&current, &stopped) || stopped.generation != current.generation {
            return Err(invalid_data(
                "endpoint changed session identity while stopping it",
            ));
        }
        Ok(stopped)
    }

    pub fn start(&self, id: &str) -> io::Result<Session> {
        let stopped = self.api.get(id)?;
        if stopped.state != SessionState::Stopped {
            return Err(io::Error::other(format!(
                "session {id} is {:?}",
                stopped.state
            )));
        }
        self.credentials(&stopped)?;
        let started = self.api.start(id)?;
        let expected_generation = stopped
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("session generation overflowed"))?;
        if !same_session_identity(&stopped, &started)
            || started.generation != expected_generation
            || started.ssh_host_public_key == stopped.ssh_host_public_key
        {
            return Err(invalid_data(
                "endpoint returned an inconsistent restarted session",
            ));
        }
        let directory = self.session_directory(id)?;
        replace_known_hosts(
            &directory,
            &started.session_id,
            &started.ssh_host_public_key,
        )?;
        Ok(started)
    }

    pub fn destroy(&self, id: &str) -> io::Result<()> {
        self.api.destroy(id)?;
        let directory = self.session_directory(id)?;
        if !directory.try_exists()? {
            return Ok(());
        }
        require_directory(&directory)?;
        remove_session_files(&directory)
    }

    fn session_directory(&self, id: &str) -> io::Result<PathBuf> {
        if !valid_session_id(id) {
            return Err(invalid_input("session ID is invalid"));
        }
        Ok(self.sessions.join(id))
    }

    fn credentials(&self, session: &Session) -> io::Result<Credentials> {
        let directory = self.session_directory(&session.session_id)?;
        require_directory(&directory)?;
        validate_requested_size(&directory, session)?;
        let private_key = directory.join(PRIVATE_KEY);
        let known_hosts = directory.join(KNOWN_HOSTS);
        require_private_file(&private_key)?;
        if !known_hosts.try_exists()? {
            finalize_pending_credentials(&directory, session)?;
        }
        require_private_file(&known_hosts)?;
        let expected = known_hosts_line(&session.session_id, &session.ssh_host_public_key);
        if read_bounded(&known_hosts, MAX_LOCAL_TEXT_BYTES)? != expected.as_bytes() {
            return Err(invalid_data(
                "session SSH host key differs from the locally pinned key",
            ));
        }
        Ok(Credentials {
            private_key,
            known_hosts,
        })
    }

    fn ready_session(&self, id: &str) -> io::Result<(Session, Credentials)> {
        let session = self.api.get(id)?;
        if session.state != SessionState::Ready {
            return Err(io::Error::other(format!(
                "session {id} is {:?}",
                session.state
            )));
        }
        let credentials = self.credentials(&session)?;
        Ok((session, credentials))
    }

    fn ssh_command(
        &self,
        session: &Session,
        credentials: &Credentials,
        tty_mode: TtyMode,
    ) -> io::Result<Command> {
        if self.api.uses_http_endpoint() && !self.api.uses_local_http_endpoint() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this experimental build has no load-balancer SSH gateway; use an SSH host endpoint",
            ));
        }

        let alias = format!("jio-{}", session.session_id);
        let target = format!("{}@{}", session.ssh_username, session.guest_ipv4);
        let mut known_hosts_option = OsString::from("UserKnownHostsFile=");
        known_hosts_option.push(&credentials.known_hosts);
        let mut command = Command::new("ssh");
        if !matches!(tty_mode, TtyMode::Disabled) {
            command.arg("-q");
        }
        command
            .arg("-i")
            .arg(&credentials.private_key)
            .arg("-o")
            .arg("IdentitiesOnly=yes")
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg("PreferredAuthentications=publickey")
            .arg("-o")
            .arg("PasswordAuthentication=no")
            .arg("-o")
            .arg("KbdInteractiveAuthentication=no")
            .arg("-o")
            .arg("HostbasedAuthentication=no")
            .arg("-o")
            .arg("StrictHostKeyChecking=yes")
            .arg("-o")
            .arg("CheckHostIP=no")
            .arg("-o")
            .arg(format!("HostKeyAlias={alias}"))
            .arg("-o")
            .arg(known_hosts_option)
            .arg("-o")
            .arg("GlobalKnownHostsFile=/dev/null")
            .arg("-o")
            .arg("UpdateHostKeys=no")
            .arg("-o")
            .arg("ForwardAgent=no")
            .arg("-o")
            .arg("ClearAllForwardings=yes")
            .arg("-o")
            .arg("PermitLocalCommand=no")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-o")
            .arg("ControlMaster=no")
            .arg("-o")
            .arg("ControlPath=none")
            .arg("-p")
            .arg(session.ssh_port.to_string());
        match tty_mode {
            TtyMode::Disabled => {
                command.arg("-T");
            }
            TtyMode::Automatic => {}
            TtyMode::Forced => {
                command.arg("-t");
            }
        }
        if !self.api.uses_local_http_endpoint() {
            command.arg("-J").arg(self.api.endpoint());
        }
        command.arg(target);
        Ok(command)
    }
}

/// A ready VM with an authenticated SSH transport prepared in the background.
///
/// The guest shell itself is already open, but its pseudoterminal remains
/// hidden until [`PreparedConnection::connect`] hands it to the local terminal.
/// Dropping this value closes that hidden shell.
pub struct PreparedConnection {
    process: Child,
    terminal: File,
    prelude: Vec<u8>,
    finished: bool,
}

impl PreparedConnection {
    /// Reveals the already-open guest shell and relays the local terminal to it.
    pub fn connect(mut self) -> io::Result<()> {
        let status = relay_terminal(&mut self.process, &self.terminal, &self.prelude)?;
        self.finished = true;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("SSH exited with {status}")))
        }
    }
}

impl Drop for PreparedConnection {
    fn drop(&mut self) {
        if !self.finished {
            terminate(&mut self.process);
        }
    }
}

/// A retained Jio VM with locally pinned SSH credentials.
#[derive(Clone)]
pub struct Vm {
    owner: VmClient,
    session: Arc<RwLock<Session>>,
}

impl Vm {
    pub fn session(&self) -> Session {
        self.cached_session()
    }

    pub fn refresh(&self) -> io::Result<Session> {
        let session = self.owner.inspect(&self.id())?;
        self.cache(session.clone());
        Ok(session)
    }

    /// Runs one command through the guest's login shell.
    pub fn exec(&self, command: &str, timeout: Duration) -> io::Result<CommandResult> {
        self.exec_with_input(command, None, timeout)
    }

    /// Runs one command and provides bounded bytes on standard input.
    pub fn exec_with_input(
        &self,
        command: &str,
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> io::Result<CommandResult> {
        validate_command(command)?;
        validate_timeout(timeout)?;
        if input.is_some_and(|bytes| bytes.len() > MAX_FILE_BYTES) {
            return Err(invalid_input(format!(
                "command input exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        let (session, credentials) = self.owner.ready_session(&self.id())?;
        self.cache(session.clone());
        let mut process = self
            .owner
            .ssh_command(&session, &credentials, TtyMode::Disabled)?;
        process.arg(command);
        run_bounded(
            process,
            input.map(<[u8]>::to_vec),
            timeout,
            MAX_COMMAND_OUTPUT_BYTES,
        )
    }

    pub fn write_file(
        &self,
        remote_path: &str,
        contents: &[u8],
        timeout: Duration,
    ) -> io::Result<()> {
        validate_remote_path(remote_path)?;
        validate_timeout(timeout)?;
        if contents.len() > MAX_FILE_BYTES {
            return Err(invalid_input(format!(
                "file exceeds {MAX_FILE_BYTES} bytes"
            )));
        }
        let command = format!("umask 077 && cat > {}", shell_quote(remote_path));
        let result = self.exec_with_input(&command, Some(contents), timeout)?;
        require_success("remote file write", &result)
    }

    pub fn read_file(&self, remote_path: &str, timeout: Duration) -> io::Result<Vec<u8>> {
        validate_remote_path(remote_path)?;
        validate_timeout(timeout)?;
        let (session, credentials) = self.owner.ready_session(&self.id())?;
        self.cache(session.clone());
        let mut process = self
            .owner
            .ssh_command(&session, &credentials, TtyMode::Disabled)?;
        process.arg(format!("cat -- {}", shell_quote(remote_path)));
        let result = run_bounded(process, None, timeout, MAX_FILE_BYTES)?;
        require_success("remote file read", &result)?;
        Ok(result.stdout)
    }

    pub fn connect(&self) -> io::Result<()> {
        let session = self.cached_session();
        if session.state != SessionState::Ready {
            return Err(io::Error::other(format!(
                "session {} is {:?}",
                session.session_id, session.state
            )));
        }
        if io::stdin().is_terminal() && io::stdout().is_terminal() && io::stderr().is_terminal() {
            return self.clone().prepare_connection()?.connect();
        }
        let credentials = self.owner.credentials(&session)?;
        let status = self
            .owner
            .ssh_command(&session, &credentials, TtyMode::Automatic)?
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("SSH exited with {status}")))
        }
    }

    /// Runs a command with inherited terminal I/O and a forced guest PTY.
    pub fn interactive_exec(&self, command: &str) -> io::Result<()> {
        validate_command(command)?;
        let (session, credentials) = self.owner.ready_session(&self.id())?;
        self.cache(session.clone());
        let status = self
            .owner
            .ssh_command(&session, &credentials, TtyMode::Forced)?
            .arg(command)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "interactive guest command exited with {status}"
            )))
        }
    }

    pub fn destroy(&self) -> io::Result<()> {
        self.owner.destroy(&self.id())?;
        let mut destroyed = self.cached_session();
        destroyed.state = SessionState::Destroyed;
        self.cache(destroyed);
        Ok(())
    }

    pub fn prepare_connection(self) -> io::Result<PreparedConnection> {
        let session = self.cached_session();
        if session.state != SessionState::Ready {
            return Err(io::Error::other(format!(
                "session {} is {:?}",
                session.session_id, session.state
            )));
        }
        let credentials = self.owner.credentials(&session)?;
        let (mut terminal, guest_terminal) = pseudoterminal()?;
        mirror_window_size(&io::stdin(), &guest_terminal)?;
        let guest_input = guest_terminal.try_clone()?;
        let guest_output = guest_terminal.try_clone()?;
        let mut command = self
            .owner
            .ssh_command(&session, &credentials, TtyMode::Forced)?;
        command
            .arg(CONNECTION_READY_COMMAND)
            .stdin(Stdio::from(guest_input))
            .stdout(Stdio::from(guest_output))
            .stderr(Stdio::from(guest_terminal));
        let mut process = command.spawn()?;
        let prelude = match wait_for_connection(&mut process, &mut terminal) {
            Ok(prelude) => prelude,
            Err(failure) => {
                terminate(&mut process);
                return Err(failure);
            }
        };
        Ok(PreparedConnection {
            process,
            terminal,
            prelude,
            finished: false,
        })
    }

    pub fn stop(&self) -> io::Result<Session> {
        let stopped = self.owner.stop(&self.id())?;
        self.cache(stopped.clone());
        Ok(stopped)
    }

    pub fn start(&self) -> io::Result<Session> {
        let started = self.owner.start(&self.id())?;
        self.cache(started.clone());
        Ok(started)
    }

    fn id(&self) -> String {
        self.cached_session().session_id
    }

    fn cached_session(&self) -> Session {
        match self.session.read() {
            Ok(session) => session.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn cache(&self, session: Session) {
        match self.session.write() {
            Ok(mut current) => *current = session,
            Err(poisoned) => *poisoned.into_inner() = session,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandResult {
    pub exit_code: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl CommandResult {
    pub fn success(&self) -> bool {
        self.exit_code == 0
    }
}

struct Credentials {
    private_key: PathBuf,
    known_hosts: PathBuf,
}

fn validate_command(command: &str) -> io::Result<()> {
    if command.is_empty() || command.len() > MAX_COMMAND_BYTES || command.as_bytes().contains(&0) {
        return Err(invalid_input(format!(
            "command must contain 1..={MAX_COMMAND_BYTES} bytes and no NUL"
        )));
    }
    Ok(())
}

fn validate_remote_path(remote_path: &str) -> io::Result<()> {
    if remote_path.is_empty()
        || remote_path.len() > MAX_REMOTE_PATH_BYTES
        || remote_path.as_bytes().contains(&0)
    {
        return Err(invalid_input(format!(
            "remote path must contain 1..={MAX_REMOTE_PATH_BYTES} bytes and no NUL"
        )));
    }
    Ok(())
}

fn validate_timeout(timeout: Duration) -> io::Result<()> {
    if timeout.is_zero() || timeout > MAX_COMMAND_TIMEOUT {
        return Err(invalid_input(format!(
            "timeout must be between 1 nanosecond and {} seconds",
            MAX_COMMAND_TIMEOUT.as_secs()
        )));
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn require_success(operation: &str, result: &CommandResult) -> io::Result<()> {
    if result.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&result.stderr);
    Err(io::Error::other(format!(
        "{operation} exited with {}: {}",
        result.exit_code,
        stderr.trim()
    )))
}

fn run_bounded(
    mut command: Command,
    input: Option<Vec<u8>>,
    timeout: Duration,
    output_limit: usize,
) -> io::Result<CommandResult> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return kill_and_error(child, "child stdout was not captured"),
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => return kill_and_error(child, "child stderr was not captured"),
    };
    let exceeded = Arc::new(AtomicBool::new(false));
    let stdout_exceeded = Arc::clone(&exceeded);
    let stderr_exceeded = Arc::clone(&exceeded);
    let stdout_thread = match thread::Builder::new()
        .name("jio-stdout".into())
        .spawn(move || drain_bounded(stdout, output_limit, stdout_exceeded))
    {
        Ok(handle) => handle,
        Err(error) => {
            terminate(&mut child);
            return Err(error);
        }
    };
    let stderr_thread = match thread::Builder::new()
        .name("jio-stderr".into())
        .spawn(move || drain_bounded(stderr, output_limit, stderr_exceeded))
    {
        Ok(handle) => handle,
        Err(error) => {
            terminate(&mut child);
            let _ = join_reader(stdout_thread);
            return Err(error);
        }
    };
    let input_thread = match (input, child.stdin.take()) {
        (Some(bytes), Some(mut stdin)) => {
            match thread::Builder::new()
                .name("jio-stdin".into())
                .spawn(move || stdin.write_all(&bytes))
            {
                Ok(handle) => Some(handle),
                Err(error) => {
                    terminate(&mut child);
                    let _ = join_reader(stdout_thread);
                    let _ = join_reader(stderr_thread);
                    return Err(error);
                }
            }
        }
        (Some(_), None) => {
            terminate(&mut child);
            let _ = join_reader(stdout_thread);
            let _ = join_reader(stderr_thread);
            return Err(io::Error::other("child stdin was not captured"));
        }
        (None, _) => None,
    };

    let started = Instant::now();
    let status = loop {
        if exceeded.load(Ordering::Relaxed) {
            terminate(&mut child);
            break Err(invalid_data(format!(
                "command output exceeded {output_limit} bytes"
            )));
        }
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => {
                terminate(&mut child);
                break Err(error);
            }
        }
        if started.elapsed() >= timeout {
            terminate(&mut child);
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "command exceeded its timeout",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    };

    let stdout = join_reader(stdout_thread)?;
    let stderr = join_reader(stderr_thread)?;
    if let Some(input_thread) = input_thread {
        match input_thread.join() {
            Ok(result) if status.is_ok() => result?,
            Ok(_) => {}
            Err(_) => return Err(io::Error::other("command input worker failed")),
        }
    }
    let status = status?;
    if exceeded.load(Ordering::Relaxed) {
        return Err(invalid_data(format!(
            "command output exceeded {output_limit} bytes"
        )));
    }
    Ok(CommandResult {
        exit_code: exit_code(status),
        stdout,
        stderr,
    })
}

fn drain_bounded(
    mut reader: impl Read,
    limit: usize,
    exceeded: Arc<AtomicBool>,
) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(output);
        }
        let remaining = limit.saturating_sub(output.len());
        let retained = remaining.min(read);
        output.extend_from_slice(&buffer[..retained]);
        if retained < read {
            exceeded.store(true, Ordering::Relaxed);
        }
    }
}

fn join_reader(handle: thread::JoinHandle<io::Result<Vec<u8>>>) -> io::Result<Vec<u8>> {
    handle
        .join()
        .map_err(|_| io::Error::other("command output worker failed"))?
}

fn terminate(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn pseudoterminal() -> io::Result<(File, File)> {
    use rustix::pty::{OpenptFlags, grantpt, openpt, ptsname, unlockpt};

    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY).map_err(io::Error::from)?;
    grantpt(&master).map_err(io::Error::from)?;
    unlockpt(&master).map_err(io::Error::from)?;
    let guest_path = ptsname(&master, Vec::new()).map_err(io::Error::from)?;
    let guest = OpenOptions::new()
        .read(true)
        .write(true)
        .open(PathBuf::from(OsString::from_vec(guest_path.into_bytes())))?;
    Ok((File::from(master), guest))
}

fn wait_for_connection(process: &mut Child, terminal: &mut File) -> io::Result<Vec<u8>> {
    use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};

    let original_flags = fcntl_getfl(&*terminal).map_err(io::Error::from)?;
    fcntl_setfl(&*terminal, original_flags | OFlags::NONBLOCK).map_err(io::Error::from)?;
    let result = (|| {
        let started = Instant::now();
        let mut prelude = Vec::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match terminal.read(&mut buffer) {
                Ok(0) => {}
                Ok(read) => {
                    prelude.extend_from_slice(&buffer[..read]);
                    if let Some(position) = find_bytes(&prelude, CONNECTION_READY_MARKER) {
                        prelude.drain(position..position + CONNECTION_READY_MARKER.len());
                        return Ok(prelude);
                    }
                    if prelude.len() > MAX_CONNECTION_PRELUDE_BYTES {
                        return Err(invalid_data(
                            "SSH emitted too much output before opening the guest shell",
                        ));
                    }
                }
                Err(failure) if failure.kind() == io::ErrorKind::WouldBlock => {}
                Err(failure) => return Err(failure),
            }
            if let Some(status) = process.try_wait()? {
                return Err(io::Error::other(format!(
                    "SSH exited before opening the guest shell with {status}"
                )));
            }
            if started.elapsed() >= CONNECTION_PREPARE_TIMEOUT {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "SSH guest shell preparation timed out",
                ));
            }
            thread::sleep(CONNECTION_PREPARE_RETRY);
        }
    })();
    let restore = fcntl_setfl(&*terminal, original_flags).map_err(io::Error::from);
    match (result, restore) {
        (Ok(prelude), Ok(())) => Ok(prelude),
        (Err(failure), _) => Err(failure),
        (Ok(_), Err(failure)) => Err(failure),
    }
}

fn find_bytes(bytes: &[u8], needle: &[u8]) -> Option<usize> {
    bytes
        .windows(needle.len())
        .position(|candidate| candidate == needle)
}

fn mirror_window_size(
    source: &impl std::os::fd::AsFd,
    target: &impl std::os::fd::AsFd,
) -> io::Result<()> {
    use rustix::termios::{tcgetwinsize, tcsetwinsize};

    let size = tcgetwinsize(source).map_err(io::Error::from)?;
    tcsetwinsize(target, size).map_err(io::Error::from)
}

fn relay_terminal(process: &mut Child, terminal: &File, prelude: &[u8]) -> io::Result<ExitStatus> {
    let mut local = RawTerminal::open()?;
    mirror_window_size(local.file(), terminal)?;
    let mut output = io::stdout().lock();
    output.write_all(prelude)?;
    output.flush()?;
    drop(output);

    let remote_output = terminal.try_clone()?;
    let output_worker = thread::spawn(move || copy_terminal_output(remote_output));
    let mut remote_input = terminal;
    let mut last_size = local.window_size()?;
    let mut input = [0_u8; 8192];
    let status = loop {
        if let Some(status) = process.try_wait()? {
            break status;
        }
        let current_size = local.window_size()?;
        if window_dimensions(&current_size) != window_dimensions(&last_size) {
            rustix::termios::tcsetwinsize(terminal, current_size).map_err(io::Error::from)?;
            last_size = current_size;
        }
        let read = local.read(&mut input)?;
        if read != 0 {
            match remote_input.write_all(&input[..read]) {
                Ok(()) => {}
                Err(failure) if is_terminal_closed(&failure) => break process.wait()?,
                Err(failure) => return Err(failure),
            }
            remote_input.flush()?;
        }
    };
    output_worker
        .join()
        .map_err(|_| io::Error::other("SSH terminal output worker failed"))??;
    Ok(status)
}

fn copy_terminal_output(mut remote: File) -> io::Result<()> {
    let output = io::stdout();
    let mut output = output.lock();
    let mut filter = LogoutFilter::new();
    let mut buffer = [0_u8; 8192];
    loop {
        match remote.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                filter.write(&mut output, &buffer[..read])?;
                output.flush()?;
            }
            Err(failure) if is_terminal_closed(&failure) => break,
            Err(failure) => return Err(failure),
        }
    }
    filter.finish(&mut output)?;
    output.flush()
}

struct LogoutFilter {
    at_line_start: bool,
    pending: Vec<u8>,
}

impl LogoutFilter {
    const LINES: [&'static [u8]; 2] = [b"logout\n", b"logout\r\n"];

    fn new() -> Self {
        Self {
            at_line_start: true,
            pending: Vec::new(),
        }
    }

    fn write(&mut self, output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
        for &byte in bytes {
            if self.pending.is_empty() && self.at_line_start && byte == b'l' {
                self.pending.push(byte);
                continue;
            }
            if !self.pending.is_empty() {
                if Self::LINES.iter().any(|line| *line == self.pending) {
                    output.write_all(&self.pending)?;
                    self.pending.clear();
                    self.at_line_start = true;
                }
                self.pending.push(byte);
                if Self::LINES
                    .iter()
                    .any(|line| line.starts_with(&self.pending))
                {
                    continue;
                }
                output.write_all(&self.pending)?;
                self.at_line_start = self.pending.last() == Some(&b'\n');
                self.pending.clear();
                continue;
            }
            output.write_all(&[byte])?;
            self.at_line_start = byte == b'\n';
        }
        Ok(())
    }

    fn finish(self, output: &mut impl Write) -> io::Result<()> {
        if Self::LINES.iter().any(|line| *line == self.pending) {
            Ok(())
        } else {
            output.write_all(&self.pending)
        }
    }
}

fn is_terminal_closed(failure: &io::Error) -> bool {
    failure.raw_os_error() == Some(rustix::io::Errno::IO.raw_os_error())
}

fn window_dimensions(size: &rustix::termios::Winsize) -> (u16, u16) {
    (size.ws_row, size.ws_col)
}

struct RawTerminal {
    terminal: File,
    original: rustix::termios::Termios,
}

impl RawTerminal {
    fn open() -> io::Result<Self> {
        use rustix::termios::{OptionalActions, SpecialCodeIndex, tcgetattr, tcsetattr};

        let terminal = OpenOptions::new().read(true).write(true).open("/dev/tty")?;
        let original = tcgetattr(&terminal).map_err(io::Error::from)?;
        let mut raw = original.clone();
        raw.make_raw();
        raw.special_codes[SpecialCodeIndex::VMIN] = 0;
        raw.special_codes[SpecialCodeIndex::VTIME] = 1;
        tcsetattr(&terminal, OptionalActions::Now, &raw).map_err(io::Error::from)?;
        Ok(Self { terminal, original })
    }

    fn file(&self) -> &File {
        &self.terminal
    }

    fn window_size(&self) -> io::Result<rustix::termios::Winsize> {
        rustix::termios::tcgetwinsize(&self.terminal).map_err(io::Error::from)
    }
}

impl Read for RawTerminal {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.terminal.read(buffer)
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        let _ = rustix::termios::tcsetattr(
            &self.terminal,
            rustix::termios::OptionalActions::Now,
            &self.original,
        );
    }
}

fn kill_and_error<T>(mut child: Child, message: &str) -> io::Result<T> {
    terminate(&mut child);
    Err(io::Error::other(message))
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
}

fn random_session_id() -> io::Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut bytes = [0_u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    let mut id = String::with_capacity(32);
    for byte in bytes {
        id.push(char::from(HEX[usize::from(byte >> 4)]));
        id.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    Ok(id)
}

fn generate_client_authority(directory: &Path) -> io::Result<String> {
    require_directory(directory)?;
    let private_key = directory.join(PRIVATE_KEY);
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(&private_key)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("ssh-keygen failed with {status}")));
    }
    require_private_file(&private_key)?;
    let public_key = String::from_utf8(read_bounded(
        &directory.join(PUBLIC_KEY),
        MAX_LOCAL_TEXT_BYTES,
    )?)
    .map_err(invalid_data)?;
    let public_key = public_key.trim().to_owned();
    if !valid_ssh_public_key(&public_key) {
        return Err(invalid_data("ssh-keygen returned an invalid public key"));
    }
    Ok(public_key)
}

fn persist_requested_size(directory: &Path, size: Option<VmSize>) -> io::Result<()> {
    if let Some(size) = size {
        write_private_file(
            &directory.join(REQUESTED_SIZE),
            format!("{}\n", size.id()).as_bytes(),
        )?;
    }
    Ok(())
}

fn validate_requested_size(directory: &Path, session: &Session) -> io::Result<()> {
    let path = directory.join(REQUESTED_SIZE);
    let expected = match fs::symlink_metadata(&path) {
        Ok(_) => {
            require_private_file(&path)?;
            let bytes = read_bounded(&path, 32)?;
            let value = std::str::from_utf8(&bytes)
                .map_err(invalid_data)?
                .strip_suffix('\n')
                .ok_or_else(|| invalid_data("locally pinned VM size has invalid framing"))?;
            value
                .parse::<VmSize>()
                .map_err(|_| invalid_data("locally pinned VM size is invalid"))?
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if session.size != Some(expected) {
        return Err(invalid_data(
            "endpoint returned a VM size different from the locally pinned request",
        ));
    }
    Ok(())
}

fn finalize_pending_credentials(directory: &Path, session: &Session) -> io::Result<()> {
    let public_path = directory.join(PUBLIC_KEY);
    let public_key = String::from_utf8(read_bounded(&public_path, MAX_LOCAL_TEXT_BYTES)?)
        .map_err(invalid_data)?;
    if !valid_ssh_public_key(public_key.trim()) {
        return Err(invalid_data("pending session public key is invalid"));
    }
    match write_known_hosts(
        &directory.join(KNOWN_HOSTS),
        &session.session_id,
        &session.ssh_host_public_key,
    ) {
        Ok(()) => {}
        Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => {}
        Err(failure) => return Err(failure),
    }
    match fs::remove_file(public_path) {
        Ok(()) => {}
        Err(failure) if failure.kind() == io::ErrorKind::NotFound => {}
        Err(failure) => return Err(failure),
    }
    File::open(directory)?.sync_all()
}

fn same_session_identity(left: &Session, right: &Session) -> bool {
    left.session_id == right.session_id
        && left.runtime == right.runtime
        && left.size == right.size
        && left.template_id == right.template_id
        && left.core_sha256 == right.core_sha256
        && left.volume_id == right.volume_id
        && left.system_files_volume_id == right.system_files_volume_id
        && left.workspace_path == right.workspace_path
}

fn create_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.mode() & 0o777 == 0o700 =>
        {
            Ok(())
        }
        Ok(_) => Err(invalid_data(format!(
            "local state path is not a private regular directory: {}",
            path.display()
        ))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            DirBuilder::new().mode(0o700).create(path)
        }
        Err(error) => Err(error),
    }
}

fn temporary_directory(parent: &Path) -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for suffix in 0..16_u8 {
        let path = parent.join(format!(
            ".create-{}-{timestamp}-{suffix}",
            std::process::id()
        ));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a local session directory",
    ))
}

fn write_known_hosts(path: &Path, id: &str, public_key: &str) -> io::Result<()> {
    write_private_file(path, known_hosts_line(id, public_key).as_bytes())
}

fn write_private_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn replace_known_hosts(directory: &Path, id: &str, public_key: &str) -> io::Result<()> {
    replace_private_file(
        directory,
        KNOWN_HOSTS,
        known_hosts_line(id, public_key).as_bytes(),
    )
}

fn replace_private_file(directory: &Path, name: &str, contents: &[u8]) -> io::Result<()> {
    require_directory(directory)?;
    let target = directory.join(name);
    match fs::symlink_metadata(&target) {
        Ok(_) => require_private_file(&target)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    for suffix in 0..16_u8 {
        let temporary = directory.join(format!(
            ".{name}-{}-{timestamp}-{suffix}",
            std::process::id()
        ));
        match write_private_file(&temporary, contents) {
            Ok(()) => {
                if let Err(failure) = fs::rename(&temporary, &target) {
                    let _ = fs::remove_file(&temporary);
                    return Err(failure);
                }
                require_private_file(&target)?;
                return File::open(directory)?.sync_all();
            }
            Err(failure) if failure.kind() == io::ErrorKind::AlreadyExists => {}
            Err(failure) => return Err(failure),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("could not allocate a temporary {name} file"),
    ))
}

fn known_hosts_line(id: &str, public_key: &str) -> String {
    format!("jio-{id} {public_key}\n")
}

fn read_bounded(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    require_regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take((limit as u64).saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(invalid_data(format!(
            "local session file exceeded {limit} bytes"
        )));
    }
    Ok(bytes)
}

fn require_regular(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "local session path is not a regular file: {}",
            path.display()
        )))
    }
}

fn require_private_file(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() && !metadata.file_type().is_symlink() && metadata.mode() & 0o777 == 0o600
    {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "local session path is not a private regular file: {}",
            path.display()
        )))
    }
}

fn require_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() && metadata.mode() & 0o777 == 0o700 {
        Ok(())
    } else {
        Err(invalid_data(format!(
            "local session path is not a private regular directory: {}",
            path.display()
        )))
    }
}

fn remove_session_files(directory: &Path) -> io::Result<()> {
    for name in [PRIVATE_KEY, PUBLIC_KEY, KNOWN_HOSTS, REQUESTED_SIZE] {
        let path = directory.join(name);
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    fs::remove_dir(directory)
}

fn invalid_input(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.to_string())
}

fn invalid_data(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        Credentials, LogoutFilter, TtyMode, VmClient, create_private_directory, drain_bounded,
        persist_requested_size, random_session_id, remove_session_files, replace_known_hosts,
        require_private_file, run_bounded, same_session_identity, shell_quote,
        validate_remote_path, validate_requested_size, write_known_hosts,
    };
    use crate::{Session, SessionClient, SessionState, VmSize};
    use std::ffi::OsStr;
    use std::io;
    use std::net::Ipv4Addr;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    const API_KEY: &str = "0123456789abcdef0123456789abcdef";

    fn ready_session() -> Session {
        Session {
            session_id: "abababababababababababababababab".into(),
            state: SessionState::Ready,
            generation: 1,
            runtime: None,
            size: None,
            vcpu_count: Some(2),
            memory_mib: Some(4096),
            template_id: "a".repeat(64),
            core_sha256: "b".repeat(64),
            volume_id: "cd".repeat(16),
            system_files_volume_id: Some("ef".repeat(16)),
            workspace_path: "/workspace".into(),
            guest_ipv4: Ipv4Addr::new(10, 0, 0, 2),
            ssh_port: 22,
            ssh_username: "jio".into(),
            ssh_host_public_key: "unused".into(),
            volume_create_ns: Some(1),
            storage_attach_ns: Some(1),
            worker_spawn_ns: Some(1),
            worker_template_prepare_ns: Some(1),
            cow_fork_ns: Some(1),
            vm_create_ns: Some(1),
            state_restore_ns: Some(1),
            device_restore_ns: Some(1),
            vsock_transport_reset_ns: Some(1),
            vsock_connect_ns: Some(1),
            vsock_init_ns: Some(1),
            guest_ready_ns: 1,
            system_files_ready_ns: Some(2),
            storage_ready_ns: 3,
            network_ready_ns: 4,
            ssh_ready_ns: 5,
            access_probe_ns: Some(1),
            worker_ready_ns: Some(6),
            session_ready_ns: Some(7),
        }
    }

    #[test]
    fn quotes_remote_paths_for_a_posix_shell() {
        assert_eq!(shell_quote("/tmp/it's here"), "'/tmp/it'\"'\"'s here'");
    }

    #[test]
    fn pins_a_session_specific_host_alias() {
        assert_eq!(
            super::known_hosts_line("ab", "ssh-ed25519 AAAA"),
            "jio-ab ssh-ed25519 AAAA\n"
        );
    }

    #[test]
    fn generates_canonical_opaque_session_ids() -> io::Result<()> {
        let id = random_session_id()?;
        assert!(crate::valid_session_id(&id));
        Ok(())
    }

    #[test]
    fn treats_runtime_as_part_of_session_identity() {
        let current = ready_session();
        let mut changed = current.clone();
        changed.runtime = Some("python3.12-source-v0".into());
        assert!(!same_session_identity(&current, &changed));

        let mut changed = current.clone();
        changed.size = Some(VmSize::Medium);
        assert!(!same_session_identity(&current, &changed));
    }

    #[test]
    fn pins_the_requested_size_until_async_creation_is_ready() -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("jio-client-size-{}", random_session_id()?));
        create_private_directory(&directory)?;
        let result = (|| {
            persist_requested_size(&directory, Some(VmSize::Medium))?;
            require_private_file(&directory.join(super::REQUESTED_SIZE))?;

            let mut session = ready_session();
            session.size = Some(VmSize::Medium);
            validate_requested_size(&directory, &session)?;
            session.size = Some(VmSize::Large);
            assert!(validate_requested_size(&directory, &session).is_err());
            Ok(())
        })();
        let cleanup = remove_session_files(&directory);
        result.and(cleanup)
    }

    #[test]
    fn stores_and_clears_the_current_session_id() -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("jio-client-current-{}", random_session_id()?));
        let client = VmClient::with_state_directory("http://127.0.0.1:8080", API_KEY, &directory)?;
        let first = "abababababababababababababababab";
        let second = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";
        let result = (|| {
            assert_eq!(client.current_session_id()?, None);
            client.set_current_session_id(first)?;
            assert_eq!(client.current_session_id()?.as_deref(), Some(first));
            require_private_file(&directory.join(super::CURRENT_SESSION))?;

            client.clear_current_session_id(second)?;
            assert_eq!(client.current_session_id()?.as_deref(), Some(first));
            client.clear_current_session_id(first)?;
            assert_eq!(client.current_session_id()?, None);
            Ok(())
        })();
        drop(client);
        let remove_sessions = std::fs::remove_dir(directory.join("sessions"));
        let remove_directory = std::fs::remove_dir(directory);
        result.and(remove_sessions).and(remove_directory)
    }

    #[test]
    fn uncertain_create_retains_local_authority_and_reports_the_id() -> io::Result<()> {
        let root = std::env::temp_dir().join(format!("jio-pending-test-{}", random_session_id()?));
        let client = VmClient::with_state_directory("http://127.0.0.1:8080", API_KEY, &root)?;
        let pending = super::temporary_directory(&client.sessions)?;
        super::write_private_file(&pending.join(super::PRIVATE_KEY), b"test-private-authority")?;
        let id = random_session_id()?;
        let error = client.retain_failed_create(
            &id,
            &pending,
            io::Error::new(io::ErrorKind::ConnectionReset, "response lost"),
        );
        assert!(error.to_string().contains(&id));
        assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        let saved = client.sessions.join(&id);
        assert_eq!(
            std::fs::read(saved.join(super::PRIVATE_KEY))?,
            b"test-private-authority"
        );
        remove_session_files(&saved)?;
        std::fs::remove_dir(client.sessions)?;
        std::fs::remove_dir(root)
    }

    #[test]
    fn replaces_the_pinned_key_for_a_new_generation() -> io::Result<()> {
        let directory =
            std::env::temp_dir().join(format!("jio-client-known-hosts-{}", random_session_id()?));
        create_private_directory(&directory)?;
        let path = directory.join(super::KNOWN_HOSTS);
        let result = (|| {
            write_known_hosts(&path, "ab", "ssh-ed25519 old")?;
            replace_known_hosts(&directory, "ab", "ssh-ed25519 new")?;
            assert_eq!(std::fs::read_to_string(path)?, "jio-ab ssh-ed25519 new\n");
            Ok(())
        })();
        let cleanup = remove_session_files(&directory);
        result.and(cleanup)
    }

    #[test]
    fn rejects_nul_in_a_remote_path() {
        assert!(validate_remote_path("/tmp/a\0b").is_err());
    }

    #[test]
    fn hides_only_a_final_logout_line() -> io::Result<()> {
        let mut hidden = Vec::new();
        let mut filter = LogoutFilter::new();
        filter.write(&mut hidden, b"exit\r\nlog")?;
        filter.write(&mut hidden, b"out\r\n")?;
        filter.finish(&mut hidden)?;
        assert_eq!(hidden, b"exit\r\n");

        let mut retained = Vec::new();
        let mut filter = LogoutFilter::new();
        filter.write(&mut retained, b"logout\r\nprompt")?;
        filter.finish(&mut retained)?;
        assert_eq!(retained, b"logout\r\nprompt");
        Ok(())
    }

    #[test]
    fn places_noninteractive_options_before_the_ssh_destination() -> io::Result<()> {
        let client = VmClient {
            api: SessionClient::new("http://127.0.0.1:8080", API_KEY)?,
            sessions: PathBuf::new(),
        };
        let session = ready_session();
        let credentials = Credentials {
            private_key: PathBuf::from("/tmp/id_ed25519"),
            known_hosts: PathBuf::from("/tmp/state with spaces/known_hosts"),
        };
        let command = client.ssh_command(&session, &credentials, TtyMode::Disabled)?;
        let arguments: Vec<_> = command.get_args().collect();
        let tty = arguments
            .iter()
            .position(|value| *value == OsStr::new("-T"));
        let destination = arguments
            .iter()
            .position(|value| *value == OsStr::new("jio@10.0.0.2"));
        assert!(tty.is_some_and(|tty| destination.is_some_and(|destination| tty < destination)));
        assert_eq!(arguments.last().copied(), Some(OsStr::new("jio@10.0.0.2")));
        assert!(arguments.iter().any(|value| {
            *value == OsStr::new("UserKnownHostsFile=/tmp/state with spaces/known_hosts")
        }));
        Ok(())
    }

    #[test]
    fn forces_a_tty_before_an_interactive_command_destination() -> io::Result<()> {
        let client = VmClient {
            api: SessionClient::new("http://127.0.0.1:8080", API_KEY)?,
            sessions: PathBuf::new(),
        };
        let session = ready_session();
        let credentials = Credentials {
            private_key: PathBuf::from("/tmp/id_ed25519"),
            known_hosts: PathBuf::from("/tmp/known_hosts"),
        };
        let command = client.ssh_command(&session, &credentials, TtyMode::Forced)?;
        let arguments: Vec<_> = command.get_args().collect();
        let tty = arguments
            .iter()
            .position(|value| *value == OsStr::new("-t"));
        let destination = arguments
            .iter()
            .position(|value| *value == OsStr::new("jio@10.0.0.2"));
        assert!(tty.is_some_and(|tty| destination.is_some_and(|destination| tty < destination)));
        Ok(())
    }

    #[test]
    fn captures_binary_input_and_output() -> io::Result<()> {
        let mut command = Command::new("sh");
        command.args(["-c", "cat"]);
        let result = run_bounded(
            command,
            Some(vec![0, 1, 2, 255]),
            Duration::from_secs(1),
            16,
        )?;
        assert_eq!(result.stdout, vec![0, 1, 2, 255]);
        assert!(result.stderr.is_empty());
        assert!(result.success());
        Ok(())
    }

    #[test]
    fn rejects_output_above_the_bound() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf 12345"]);
        assert!(
            run_bounded(command, None, Duration::from_secs(1), 4)
                .is_err_and(|error| error.kind() == io::ErrorKind::InvalidData)
        );
    }

    #[test]
    fn bounded_reader_retains_only_the_limit() -> io::Result<()> {
        let exceeded = Arc::new(AtomicBool::new(false));
        let output = drain_bounded(&b"abcdef"[..], 3, Arc::clone(&exceeded))?;
        assert_eq!(output, b"abc");
        assert!(exceeded.load(std::sync::atomic::Ordering::Relaxed));
        Ok(())
    }
}
