use std::io;
use std::process::Command;

#[test]
fn version_is_offline_and_matches_the_package() -> io::Result<()> {
    for flag in ["--version", "-V"] {
        let output = Command::new(env!("CARGO_BIN_EXE_jio"))
            .arg(flag)
            .env_remove("JIO_API_KEY")
            .env("JIO_ENDPOINT", "invalid endpoint")
            .output()?;
        assert!(output.status.success());
        assert_eq!(
            output.stdout,
            concat!("jio ", env!("CARGO_PKG_VERSION"), "\n").as_bytes()
        );
        assert!(output.stderr.is_empty());
    }
    assert!(
        !Command::new(env!("CARGO_BIN_EXE_jio"))
            .args(["--version", "extra"])
            .output()?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn no_connection_setup_is_required_but_an_api_key_still_is() -> io::Result<()> {
    let directory = TestDirectory::new("missing-login")?;
    let output = Command::new(env!("CARGO_BIN_EXE_jio"))
        .arg("usage")
        .env("JIO_STATE_DIR", &directory.0)
        .env_remove("JIO_ENDPOINT")
        .env_remove("JIO_HOST")
        .env_remove("JIO_CA_CERT")
        .env_remove("JIO_API_KEY")
        .output()?;
    assert!(!output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "jio: not logged in; run jio login <api-key> or set JIO_API_KEY\n"
    );
    assert!(output.stdout.is_empty());
    Ok(())
}

struct TestDirectory(std::path::PathBuf);

impl TestDirectory {
    fn new(name: &str) -> io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(format!("jio-{name}-{}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        Ok(Self(path))
    }

    fn command(&self, endpoint: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_jio"));
        command
            .env("JIO_STATE_DIR", &self.0)
            .env("JIO_ENDPOINT", endpoint)
            .env_remove("JIO_API_KEY")
            .env_remove("JIO_CA_CERT");
        command
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn login_persists_verified_credentials_and_fails_closed() -> io::Result<()> {
    use std::fs;
    use std::io::{BufRead, Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{Duration, Instant};

    let directory = TestDirectory::new("login")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let key = "a".repeat(64);
    let replacement = "b".repeat(64);
    let server = std::thread::spawn(move || -> io::Result<()> {
        for (path, key, status) in [
            ("/v1/usage", 'a', 200), // Login verifies the argument, not JIO_API_KEY.
            ("/v1/usage", 'a', 200), // A later process uses the saved key.
            ("/v1/sessions", 'a', 200),
            ("/v1/usage", 'b', 401), // Failed login preserves the saved key.
            ("/v1/usage", 'b', 200), // Environment overrides the saved key.
            ("/v1/usage", 'b', 200), // Successful replacement.
            ("/v1/usage", 'b', 200),
        ] {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => return Err(error),
                }
            };
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut reader = io::BufReader::new((&stream).take(8192));
            let mut headers = String::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line)? == 0 || line == "\r\n" {
                    break;
                }
                headers.push_str(&line);
            }
            assert!(headers.starts_with(&format!("GET {path}")));
            assert!(headers.to_ascii_lowercase().contains(&format!(
                "authorization: bearer {}\r\n",
                key.to_string().repeat(64)
            )));
            let body = if status == 401 {
                r#"{"error":"invalid API key"}"#
            } else if path == "/v1/sessions" {
                r#"{"sessions":[],"next_after":null}"#
            } else {
                r#"{"account_id":"demo","limits":{"cpu":8,"memory_mib":16384,"disk_mib":131072},"reserved":{"cpu":0,"memory_mib":0,"disk_mib":0},"compute_sessions":0,"retained_sessions":0,"session_ttl_seconds":1800}"#
            };
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        }
        Ok(())
    });

    let config = directory.0.join("config");
    fs::write(&config, b"size=medium\n")?;
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600))?;
    let output = directory
        .command(&endpoint)
        .args(["login", &key])
        .env("JIO_API_KEY", "old-invalid-key")
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("Logged in to Jio as demo."));
    assert!(String::from_utf8_lossy(&output.stdout).contains("overrides"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&key));
    let credentials = directory.0.join("credentials");
    let saved = fs::read(&credentials)?;
    assert_eq!(
        fs::metadata(&credentials)?.permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(fs::read(&config)?, b"size=medium\n");
    for command in ["usage", "list"] {
        let output = directory.command(&endpoint).arg(command).output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    for invalid in [
        "short".to_owned(),
        "x".repeat(257),
        format!("{}\n", "x".repeat(64)),
    ] {
        let output = directory
            .command(&endpoint)
            .args(["login", &invalid])
            .output()?;
        assert!(!output.status.success());
        assert_eq!(fs::read(&credentials)?, saved);
    }
    let output = directory
        .command(&endpoint)
        .args(["login", &replacement])
        .output()?;
    assert!(!output.status.success());
    assert_eq!(fs::read(&credentials)?, saved);
    assert!(
        directory
            .command(&endpoint)
            .arg("usage")
            .env("JIO_API_KEY", &replacement)
            .output()?
            .status
            .success()
    );
    assert!(
        !directory
            .command(&endpoint)
            .arg("usage")
            .env("JIO_API_KEY", "")
            .output()?
            .status
            .success()
    );
    let output = directory
        .command("http://127.0.0.1:1")
        .arg("usage")
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("different endpoint"));
    assert!(
        directory
            .command(&endpoint)
            .args(["login", &replacement])
            .output()?
            .status
            .success()
    );
    assert!(
        directory
            .command(&endpoint)
            .arg("usage")
            .output()?
            .status
            .success()
    );
    assert_ne!(fs::read(&credentials)?, saved);
    server
        .join()
        .map_err(|_| io::Error::other("mock server failed"))??;

    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o644))?;
    assert!(
        !directory
            .command(&endpoint)
            .arg("usage")
            .output()?
            .status
            .success()
    );
    fs::set_permissions(&credentials, fs::Permissions::from_mode(0o600))?;
    for malformed in ["x".repeat(4097), format!("{endpoint}\n{key}\nextra\n")] {
        fs::write(&credentials, malformed)?;
        assert!(
            !directory
                .command(&endpoint)
                .arg("usage")
                .output()?
                .status
                .success()
        );
    }
    fs::remove_file(&credentials)?;
    symlink(&config, &credentials)?;
    assert!(
        !directory
            .command(&endpoint)
            .arg("usage")
            .output()?
            .status
            .success()
    );
    assert_eq!(fs::read(&config)?, b"size=medium\n");
    Ok(())
}
