use jio_client::{CommandResult, PreparedConnection, Session, VmClient, VmSize};
use std::env;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

const MAX_CODEX_AUTH_BYTES: u64 = 64 * 1024;
const CODEX_YOLO_COMMAND: &str = concat!(
    "cd /workspace && exec codex ",
    "--profile jio-yolo ",
    "--dangerously-bypass-approvals-and-sandbox",
);
const CODEX_YOLO_PROFILE: &[u8] = b"[projects.\"/workspace\"]\ntrust_level = \"trusted\"\n";
const INSTALL_CODEX_YOLO_PROFILE: &str = concat!(
    "set -eu\n",
    "umask 077\n",
    "install -d -m 0700 \"$HOME/.codex\"\n",
    "temporary=$(mktemp \"$HOME/.codex/.jio-yolo.config.toml.XXXXXX\")\n",
    "trap 'rm -f \"$temporary\"' 0 1 2 15\n",
    "cat > \"$temporary\"\n",
    "chmod 0600 \"$temporary\"\n",
    "mv -f \"$temporary\" \"$HOME/.codex/jio-yolo.config.toml\"\n",
    "trap - 0 1 2 15",
);
const INSTALL_CODEX_AUTH: &str = concat!(
    "set -eu\n",
    "umask 077\n",
    "install -d -m 0700 \"$HOME/.codex\"\n",
    "temporary=$(mktemp \"$HOME/.codex/.auth.json.XXXXXX\")\n",
    "trap 'rm -f \"$temporary\"' 0 1 2 15\n",
    "cat > \"$temporary\"\n",
    "chmod 0600 \"$temporary\"\n",
    "mv -f \"$temporary\" \"$HOME/.codex/auth.json\"\n",
    "trap - 0 1 2 15",
);

pub fn create(host: String, size: VmSize) -> io::Result<Session> {
    let client = client(host)?;
    let session = client.create_with_size(size)?.session();
    client.set_current_session_id(&session.session_id)?;
    Ok(session)
}

pub fn accept_create_and_report_id(
    host: String,
    size: VmSize,
    report: impl FnOnce(&str),
) -> io::Result<String> {
    let client = client(host)?;
    let id = client.accept_create_with_size_and_report_id(size, report)?;
    client.set_current_session_id(&id)?;
    Ok(id)
}

pub fn prepare_connect(host: String, id: &str) -> io::Result<PreparedConnection> {
    client(host)?.attach(id)?.prepare_connection()
}

pub fn connect(host: String, id: Option<&str>) -> io::Result<()> {
    let client = client(host)?;
    let id = resolve_session_id(&client, id)?;
    client.attach(&id)?.connect()?;
    client.set_current_session_id(&id)
}

pub fn exec(host: String, id: &str, command: &str, timeout: Duration) -> io::Result<CommandResult> {
    client(host)?.attach(id)?.exec(command, timeout)
}

pub fn codex_login(host: String, id: Option<&str>) -> io::Result<(String, CommandResult)> {
    let client = client(host)?;
    let id = resolve_session_id(&client, id)?;
    let vm = client.attach(&id)?;
    Ok((id, install_codex_login(&vm)?))
}

pub fn codex_yolo(host: String, id: Option<&str>) -> io::Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "jio yolo codex requires an interactive terminal",
        ));
    }
    let client = client(host)?;
    let id = resolve_session_id(&client, id)?;
    let vm = client.attach(&id)?;
    install_codex_login(&vm)?;
    install_codex_yolo_profile(&vm)?;
    vm.interactive_exec(CODEX_YOLO_COMMAND)
}

fn install_codex_yolo_profile(vm: &jio_client::Vm) -> io::Result<()> {
    let installed = vm.exec_with_input(
        INSTALL_CODEX_YOLO_PROFILE,
        Some(CODEX_YOLO_PROFILE),
        jio_client::DEFAULT_COMMAND_TIMEOUT,
    )?;
    require_success("Codex Jio profile install", &installed)
}

fn install_codex_login(vm: &jio_client::Vm) -> io::Result<CommandResult> {
    let available = vm.exec(
        "command -v codex >/dev/null 2>&1",
        jio_client::DEFAULT_COMMAND_TIMEOUT,
    )?;
    if !available.success() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Codex is not installed in this VM",
        ));
    }

    let mut auth = read_codex_auth(&codex_auth_path()?)?;
    let installed = vm.exec_with_input(
        INSTALL_CODEX_AUTH,
        Some(&auth),
        jio_client::DEFAULT_COMMAND_TIMEOUT,
    );
    auth.fill(0);
    require_success("Codex login install", &installed?)?;

    let status = vm.exec("codex login status", jio_client::DEFAULT_COMMAND_TIMEOUT)?;
    require_success("Codex login verification", &status)?;
    Ok(status)
}

pub fn destroy(host: String, id: &str) -> io::Result<()> {
    let client = client(host)?;
    client.destroy(id)?;
    client.clear_current_session_id(id)
}

pub fn target_session_id(host: &str, id: Option<&str>) -> io::Result<String> {
    let client = client(host.to_owned())?;
    resolve_session_id(&client, id)
}

pub fn stop(host: String, id: &str) -> io::Result<Session> {
    client(host)?.stop(id)
}

pub fn start(host: String, id: &str) -> io::Result<Session> {
    client(host)?.start(id)
}

fn client(host: String) -> io::Result<VmClient> {
    let api_key = env::var("JIO_API_KEY")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "JIO_API_KEY is required"))?;
    VmClient::new(host, api_key)
}

fn resolve_session_id(client: &VmClient, id: Option<&str>) -> io::Result<String> {
    if let Some(id) = id {
        if !jio_client::valid_session_id(id) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "session ID is invalid",
            ));
        }
        return Ok(id.to_owned());
    }
    client.current_session_id()?.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "no current session; run jio create or jio connect <session-id>",
        )
    })
}

fn codex_auth_path() -> io::Result<PathBuf> {
    let directory = match env::var_os("CODEX_HOME") {
        Some(directory) if !directory.is_empty() => PathBuf::from(directory),
        Some(_) => return Err(invalid("CODEX_HOME must not be empty")),
        None => env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("HOME or CODEX_HOME is required"))?
            .join(".codex"),
    };
    Ok(directory.join("auth.json"))
}

fn read_codex_auth(path: &Path) -> io::Result<Vec<u8>> {
    let expected = fs::symlink_metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot read local Codex login at {}: {error}",
                path.display()
            ),
        )
    })?;
    if !expected.is_file() || expected.file_type().is_symlink() {
        return Err(invalid(format!(
            "local Codex login is not a regular file: {}",
            path.display()
        )));
    }

    let parent = path
        .parent()
        .ok_or_else(|| invalid("local Codex login has no parent directory"))?;
    let parent_metadata = fs::symlink_metadata(parent)?;
    if !parent_metadata.is_dir() || parent_metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "local Codex home is not a regular directory: {}",
            parent.display()
        )));
    }

    let file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file()
        || opened.dev() != expected.dev()
        || opened.ino() != expected.ino()
        || opened.uid() != parent_metadata.uid()
    {
        return Err(invalid("local Codex login changed while it was opened"));
    }
    if opened.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "local Codex login must not be accessible by group or others: {}",
                path.display()
            ),
        ));
    }

    let mut auth = Vec::new();
    file.take(MAX_CODEX_AUTH_BYTES + 1).read_to_end(&mut auth)?;
    if auth.is_empty() {
        return Err(invalid("local Codex login is empty"));
    }
    if auth.len() as u64 > MAX_CODEX_AUTH_BYTES {
        auth.fill(0);
        return Err(invalid(format!(
            "local Codex login exceeds {MAX_CODEX_AUTH_BYTES} bytes"
        )));
    }
    Ok(auth)
}

fn require_success(operation: &str, result: &CommandResult) -> io::Result<()> {
    if result.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{operation} exited with status {}",
        result.exit_code
    )))
}

pub fn host(value: Option<OsString>) -> io::Result<String> {
    value
        .map(|value| {
            value
                .into_string()
                .map_err(|_| invalid("host is not UTF-8"))
        })
        .transpose()?
        .or_else(|| env::var("JIO_ENDPOINT").ok())
        .or_else(|| env::var("JIO_HOST").ok())
        .ok_or_else(|| invalid("missing --host <host> (or JIO_ENDPOINT/JIO_HOST)"))
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::{read_codex_auth, require_success};
    use jio_client::CommandResult;
    use std::fs::{self, OpenOptions, Permissions};
    use std::io::{self, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory() -> io::Result<PathBuf> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("jio-cli-codex-auth-{}-{nonce}", std::process::id()));
        fs::create_dir(&directory)?;
        Ok(directory)
    }

    #[test]
    fn reads_only_a_private_codex_login_file() -> io::Result<()> {
        let directory = temporary_directory()?;
        let path = directory.join("auth.json");
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)?;
            file.write_all(br#"{"auth_mode":"chatgpt"}"#)?;
            drop(file);

            assert_eq!(read_codex_auth(&path)?, br#"{"auth_mode":"chatgpt"}"#);
            fs::set_permissions(&path, Permissions::from_mode(0o644))?;
            assert!(read_codex_auth(&path).is_err());
            Ok(())
        })();
        let remove_file = fs::remove_file(&path);
        let remove_directory = fs::remove_dir(&directory);
        result.and(remove_file).and(remove_directory)
    }

    #[test]
    fn credential_verification_errors_do_not_echo_guest_output() {
        let result = CommandResult {
            exit_code: 1,
            stdout: b"sensitive stdout".to_vec(),
            stderr: b"sensitive stderr".to_vec(),
        };
        let message = require_success("Codex login verification", &result)
            .err()
            .map(|error| error.to_string());
        assert_eq!(
            message.as_deref(),
            Some("Codex login verification exited with status 1")
        );
    }
}
