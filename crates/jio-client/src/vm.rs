use crate::{Session, SessionClient, SessionState, valid_session_id, valid_ssh_public_key};
use std::env;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const DEFAULT_RUNTIME: &str = "python3.12-source-v0";
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
/// let vm = client.create_default()?;
/// let result = vm.exec("python -c 'print(6 * 7)'", DEFAULT_COMMAND_TIMEOUT)?;
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

    pub fn create_default(&self) -> io::Result<Vm> {
        self.create(DEFAULT_RUNTIME)
    }

    pub fn create(&self, runtime: &str) -> io::Result<Vm> {
        let temporary = temporary_directory(&self.sessions)?;
        let result = self.create_inner(runtime, &temporary);
        if result.is_err() {
            let _ = remove_session_files(&temporary);
        }
        result
    }

    fn create_inner(&self, runtime: &str, temporary: &Path) -> io::Result<Vm> {
        let private_key = temporary.join(PRIVATE_KEY);
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
        let public_path = temporary.join(PUBLIC_KEY);
        let public_key = String::from_utf8(read_bounded(&public_path, MAX_LOCAL_TEXT_BYTES)?)
            .map_err(invalid_data)?;
        let public_key = public_key.trim().to_owned();
        if !valid_ssh_public_key(&public_key) {
            return Err(invalid_data("ssh-keygen returned an invalid public key"));
        }

        let session = self.api.create(runtime, &public_key)?;
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
            session,
        })
    }

    /// Returns lifecycle metadata without requiring this machine to own the SSH key.
    pub fn inspect(&self, id: &str) -> io::Result<Session> {
        self.api.get(id)
    }

    /// Attaches a programmable handle using credentials previously created locally.
    pub fn attach(&self, id: &str) -> io::Result<Vm> {
        let session = self.api.get(id)?;
        self.credentials(&session)?;
        Ok(Vm {
            owner: self.clone(),
            session,
        })
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
        let private_key = directory.join(PRIVATE_KEY);
        let known_hosts = directory.join(KNOWN_HOSTS);
        require_private_file(&private_key)?;
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
        disable_tty: bool,
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
            .arg("ControlMaster=no")
            .arg("-o")
            .arg("ControlPath=none")
            .arg("-o")
            .arg("ForwardAgent=no")
            .arg("-o")
            .arg("ClearAllForwardings=yes")
            .arg("-o")
            .arg("PermitLocalCommand=no")
            .arg("-o")
            .arg("ConnectTimeout=10")
            .arg("-p")
            .arg(session.ssh_port.to_string());
        if disable_tty {
            command.arg("-T");
        }
        if !self.api.uses_local_http_endpoint() {
            command.arg("-J").arg(self.api.endpoint());
        }
        command.arg(target);
        Ok(command)
    }
}

/// A retained Jio VM with locally pinned SSH credentials.
#[derive(Clone)]
pub struct Vm {
    owner: VmClient,
    session: Session,
}

impl Vm {
    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn refresh(&self) -> io::Result<Session> {
        self.owner.inspect(&self.session.session_id)
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
        let (session, credentials) = self.owner.ready_session(&self.session.session_id)?;
        let mut process = self.owner.ssh_command(&session, &credentials, true)?;
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
        let (session, credentials) = self.owner.ready_session(&self.session.session_id)?;
        let mut process = self.owner.ssh_command(&session, &credentials, true)?;
        process.arg(format!("cat -- {}", shell_quote(remote_path)));
        let result = run_bounded(process, None, timeout, MAX_FILE_BYTES)?;
        require_success("remote file read", &result)?;
        Ok(result.stdout)
    }

    pub fn connect(&self) -> io::Result<()> {
        let (session, credentials) = self.owner.ready_session(&self.session.session_id)?;
        let status = self
            .owner
            .ssh_command(&session, &credentials, false)?
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("SSH exited with {status}")))
        }
    }

    pub fn destroy(&self) -> io::Result<()> {
        self.owner.destroy(&self.session.session_id)
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

fn kill_and_error<T>(mut child: Child, message: &str) -> io::Result<T> {
    terminate(&mut child);
    Err(io::Error::other(message))
}

fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(-1)
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
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(known_hosts_line(id, public_key).as_bytes())?;
    file.sync_all()
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
    for name in [PRIVATE_KEY, PUBLIC_KEY, KNOWN_HOSTS] {
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
        Credentials, VmClient, drain_bounded, run_bounded, shell_quote, validate_remote_path,
    };
    use crate::{Session, SessionClient, SessionState};
    use std::ffi::OsStr;
    use std::io;
    use std::net::Ipv4Addr;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    const API_KEY: &str = "0123456789abcdef0123456789abcdef";

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
    fn rejects_nul_in_a_remote_path() {
        assert!(validate_remote_path("/tmp/a\0b").is_err());
    }

    #[test]
    fn places_noninteractive_options_before_the_ssh_destination() -> io::Result<()> {
        let client = VmClient {
            api: SessionClient::new("http://127.0.0.1:8080", API_KEY)?,
            sessions: PathBuf::new(),
        };
        let session = Session {
            session_id: "abababababababababababababababab".into(),
            state: SessionState::Ready,
            runtime: "python3.12-source-v0".into(),
            template_id: "a".repeat(64),
            core_sha256: "b".repeat(64),
            guest_ipv4: Ipv4Addr::new(10, 0, 0, 2),
            ssh_port: 22,
            ssh_username: "jio".into(),
            ssh_host_public_key: "unused".into(),
            guest_ready_ns: 1,
            network_ready_ns: 2,
            ssh_ready_ns: 3,
        };
        let credentials = Credentials {
            private_key: PathBuf::from("/tmp/id_ed25519"),
            known_hosts: PathBuf::from("/tmp/state with spaces/known_hosts"),
        };
        let command = client.ssh_command(&session, &credentials, true)?;
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
