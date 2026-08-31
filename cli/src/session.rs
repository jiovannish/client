use jio_client::{Session, SessionClient, SessionState};
use std::env;
use std::ffi::OsString;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

const PRIVATE_KEY: &str = "id_ed25519";
const PUBLIC_KEY: &str = "id_ed25519.pub";
const KNOWN_HOSTS: &str = "known_hosts";
const MAX_LOCAL_TEXT_BYTES: u64 = 1024;

pub fn create(host: String, runtime: &str) -> io::Result<Session> {
    let api = api(host)?;
    let sessions = sessions_directory()?;
    let temporary = temporary_directory(&sessions)?;
    let result = create_inner(&api, runtime, &sessions, &temporary);
    if result.is_err() {
        let _ = remove_session_files(&temporary);
    }
    result
}

fn create_inner(
    api: &SessionClient,
    runtime: &str,
    sessions: &Path,
    temporary: &Path,
) -> io::Result<Session> {
    let private_key = temporary.join(PRIVATE_KEY);
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-C", "", "-f"])
        .arg(&private_key)
        .stdin(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::other(format!("ssh-keygen failed with {status}")));
    }
    require_private_file(&private_key)?;
    let public_path = temporary.join(PUBLIC_KEY);
    let public_key = read_bounded(&public_path)?;
    let public_key = public_key.trim().to_owned();
    if !jio_client::valid_ssh_public_key(&public_key) {
        return Err(invalid("ssh-keygen returned an invalid public key"));
    }

    let session = api.create(runtime, &public_key)?;
    let finalize = (|| {
        write_known_hosts(
            &temporary.join(KNOWN_HOSTS),
            &session.session_id,
            &session.ssh_host_public_key,
        )?;
        fs::remove_file(public_path)?;
        let target = sessions.join(&session.session_id);
        if target.try_exists()? {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "local session directory already exists",
            ));
        }
        fs::rename(temporary, target)
    })();
    if let Err(error) = finalize {
        let _ = api.destroy(&session.session_id);
        return Err(error);
    }
    Ok(session)
}

pub fn connect(host: String, id: &str) -> io::Result<()> {
    let api = api(host)?;
    if api.uses_http_endpoint() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this experimental build has no load-balancer SSH gateway; use --host user@address",
        ));
    }
    let session = api.get(id)?;
    if session.state != SessionState::Ready {
        return Err(io::Error::other(format!(
            "session {id} is {:?}",
            session.state
        )));
    }
    let directory = sessions_directory()?.join(id);
    require_directory(&directory)?;
    let private_key = directory.join(PRIVATE_KEY);
    let known_hosts = directory.join(KNOWN_HOSTS);
    require_private_file(&private_key)?;
    require_private_file(&known_hosts)?;
    let expected = known_hosts_line(id, &session.ssh_host_public_key);
    if read_bounded(&known_hosts)? != expected {
        return Err(invalid(
            "session SSH host key differs from the locally pinned key",
        ));
    }

    let alias = format!("jio-{id}");
    let target = format!("{}@{}", session.ssh_username, session.guest_ipv4);
    let status = Command::new("ssh")
        .arg("-i")
        .arg(&private_key)
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
        .arg(format!("UserKnownHostsFile={}", known_hosts.display()))
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
        .arg("-p")
        .arg(session.ssh_port.to_string())
        .arg("-J")
        .arg(api.endpoint())
        .arg(target)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("SSH exited with {status}")))
    }
}

pub fn destroy(host: String, id: &str) -> io::Result<()> {
    let api = api(host)?;
    api.destroy(id)?;
    let directory = sessions_directory()?.join(id);
    if !directory.try_exists()? {
        return Ok(());
    }
    require_directory(&directory)?;
    remove_session_files(&directory)
}

fn api(host: String) -> io::Result<SessionClient> {
    let api_key = env::var("JIO_API_KEY")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "JIO_API_KEY is required"))?;
    SessionClient::new(host, api_key)
}

fn sessions_directory() -> io::Result<PathBuf> {
    let root = match env::var_os("JIO_STATE_DIR") {
        Some(path) => PathBuf::from(path),
        None => env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| invalid("HOME or JIO_STATE_DIR is required"))?
            .join(".jio"),
    };
    create_private_directory(&root)?;
    let sessions = root.join("sessions");
    create_private_directory(&sessions)?;
    Ok(sessions)
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
        Ok(_) => Err(invalid(format!(
            "local state path is not a regular directory: {}",
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
    let path = parent.join(format!(".create-{}-{timestamp}", std::process::id()));
    DirBuilder::new().mode(0o700).create(&path)?;
    Ok(path)
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

fn read_bounded(path: &Path) -> io::Result<String> {
    require_regular(path)?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_LOCAL_TEXT_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_LOCAL_TEXT_BYTES {
        return Err(invalid("local session file exceeded its bound"));
    }
    String::from_utf8(bytes).map_err(invalid)
}

fn require_regular(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_file() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(invalid(format!(
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
        Err(invalid(format!(
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
        Err(invalid(format!(
            "local session path is not a regular directory: {}",
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

pub fn runtime(language: &str) -> io::Result<&'static str> {
    match language {
        "python" => Ok("python3.12-source-v0"),
        _ => Err(invalid(format!("unsupported language: {language}"))),
    }
}

pub fn host(value: Option<OsString>) -> io::Result<String> {
    value
        .map(|value| {
            value
                .into_string()
                .map_err(|_| invalid("host is not UTF-8"))
        })
        .transpose()?
        .or_else(|| env::var("JIO_HOST").ok())
        .ok_or_else(|| invalid("missing --host <host> (or JIO_HOST)"))
}

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::{known_hosts_line, runtime};

    #[test]
    fn pins_a_session_specific_host_alias() {
        assert_eq!(
            known_hosts_line("ab", "ssh-ed25519 AAAA"),
            "jio-ab ssh-ed25519 AAAA\n"
        );
    }

    #[test]
    fn maps_only_the_session_template_language() {
        assert_eq!(runtime("python").ok(), Some("python3.12-source-v0"));
        assert!(runtime("rust").is_err());
    }
}
