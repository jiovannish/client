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

#[test]
fn aliases_resolve_to_ids_before_vm_operations() -> io::Result<()> {
    use std::io::{BufRead, Read, Write};
    use std::time::{Duration, Instant};
    let directory = TestDirectory::new("aliases")?;
    let id = "a".repeat(32);
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let server_id = id.clone();
    let server = std::thread::spawn(move || -> io::Result<()> {
        let requests = [
            ("PUT", format!("/v1/sessions/{server_id}/alias"), 200),
            ("GET", "/v1/aliases/dev".into(), 200),
            ("POST", format!("/v0/sessions/{server_id}/exec"), 200),
            ("GET", "/v1/aliases/dev".into(), 200),
            ("GET", format!("/v1/sessions/{server_id}/ports"), 200),
            ("GET", "/v1/aliases/dev".into(), 200),
            ("GET", format!("/v0/sessions/{server_id}"), 404), // stop
            ("GET", "/v1/aliases/dev".into(), 200),
            ("GET", format!("/v0/sessions/{server_id}"), 404), // start
            ("GET", "/v1/aliases/dev".into(), 200),
            ("GET", format!("/v0/sessions/{server_id}"), 404), // connect
            ("GET", "/v1/aliases/dev".into(), 200),
            ("DELETE", format!("/v0/sessions/{server_id}"), 204),
            ("GET", "/v1/aliases/missing".into(), 404),
        ];
        for (method, path, status) in requests {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) => return Err(e),
                }
            };
            stream.set_nonblocking(false)?;
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
            assert!(
                headers.starts_with(&format!("{method} {path} HTTP/1.1\r\n")),
                "{headers}"
            );
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains(&format!("authorization: bearer {}", "b".repeat(64)))
            );
            let length: usize = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length: ")
                        .map(str::to_owned)
                })
                .unwrap_or_else(|| "0".into())
                .parse()
                .map_err(io::Error::other)?;
            assert!(length < 4096);
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes)?;
            let request = String::from_utf8(bytes).map_err(io::Error::other)?;
            let body = if status == 404 {
                r#"{"error":"not found"}"#.to_owned()
            } else if path.ends_with("/exec") {
                let nonce = request
                    .split("\"request_id\":\"")
                    .nth(1)
                    .and_then(|value| value.split('"').next())
                    .ok_or_else(|| io::Error::other("missing nonce"))?;
                assert!(jio_client::valid_session_id(nonce));
                format!(
                    r#"{{"request_id":"{nonce}","stdout":[111,107,10],"stderr":[],"exit_code":0,"signal":null,"timed_out":false,"output_limit":false}}"#
                )
            } else if path.ends_with("/ports") {
                "[]".into()
            } else if method == "DELETE" {
                String::new()
            } else {
                if method == "PUT" {
                    assert_eq!(request, r#"{"alias":"dev"}"#);
                }
                format!(r#"{{"session_id":"{server_id}","alias":"dev"}}"#)
            };
            write!(
                stream,
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )?;
        }
        Ok(())
    });
    for (args, success) in [
        (vec!["alias", &id, "dev"], true),
        (vec!["exec", "dev", "printf ok"], true),
        (vec!["ports", "dev"], true),
        (vec!["stop", "dev"], false),
        (vec!["start", "dev"], false),
        (vec!["connect", "dev"], false),
        (vec!["destroy", "dev", "--yes"], true),
        (vec!["destroy", "missing", "--yes"], false),
    ] {
        let output = directory
            .command(&endpoint)
            .args(&args)
            .env("JIO_API_KEY", "b".repeat(64))
            .output()?;
        assert_eq!(
            output.status.success(),
            success,
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if args[0] == "exec" {
            assert_eq!(output.stdout, b"ok\n");
        }
    }
    server
        .join()
        .map_err(|_| io::Error::other("alias test endpoint failed"))??;
    Ok(())
}

#[test]
fn invalid_domain_never_contacts_the_endpoint() -> io::Result<()> {
    let directory = TestDirectory::new("invalid-domain")?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    for name in ["invalid", "../example.com", "bad..example.com"] {
        let output = directory
            .command(&endpoint)
            .args(["expose", "3000", &"a".repeat(32), "--domain", name])
            .env("JIO_API_KEY", "b".repeat(64))
            .output()?;
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("invalid DNS hostname"));
        assert!(output.stdout.is_empty());
        assert!(
            listener
                .accept()
                .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
        );
    }
    Ok(())
}

#[test]
#[cfg(unix)]
fn missing_openssh_reports_the_requirement_before_creating_a_vm() -> io::Result<()> {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::time::{Duration, Instant};

    let directory = TestDirectory::new("missing-openssh")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let mut child = directory
        .command(&endpoint)
        .arg("create")
        .env("JIO_API_KEY", "a".repeat(64))
        .env("PATH", &directory.0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let result = (|| -> io::Result<()> {
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(connection) => break connection,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => return Err(error),
            }
        };
        // Accepted sockets inherit the listener's nonblocking mode on macOS.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut request = [0; 8192];
        let count = stream.read(&mut request)?;
        assert!(request[..count].starts_with(b"GET /v0/health "));
        let body = r#"{"status":"ready","sizes":["medium"]}"#;
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )?;
        drop(stream);
        while child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "create did not exit",
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = child.kill();
    }
    let output = child.wait_with_output()?;
    result?;
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("could not run ssh-keygen; install OpenSSH")
    );
    assert!(
        listener
            .accept()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock)
    );
    Ok(())
}

impl TestDirectory {
    fn new(name: &str) -> io::Result<Self> {
        #[cfg(unix)]
        use std::os::unix::fs::DirBuilderExt;
        let path = std::env::temp_dir().join(format!("jio-{name}-{}", std::process::id()));
        #[cfg(unix)]
        std::fs::DirBuilder::new().mode(0o700).create(&path)?;
        #[cfg(windows)]
        jio_client::windows::create_private_directory(&path)?;
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
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt, symlink};
    use std::time::{Duration, Instant};

    let directory = TestDirectory::new("login")?;
    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let key = "a".repeat(64);
    let replacement = "b".repeat(64);
    let ports_path = format!("/v1/sessions/{}/ports", "e".repeat(32));
    let server = std::thread::spawn(move || -> io::Result<()> {
        for (path, key, status) in [
            ("/v1/usage", 'a', 200), // Login verifies the argument, not JIO_API_KEY.
            ("/v1/usage", 'a', 200), // A later process uses the saved key.
            ("/v1/sessions", 'a', 200),
            (ports_path.as_str(), 'a', 200),
            ("/v1/usage", 'b', 401), // Failed login preserves the saved key.
            ("/v1/usage", 'b', 200), // Environment overrides the saved key.
            ("/v1/usage", 'b', 200), // Successful replacement.
            ("/v1/usage", 'b', 200),
        ] {
            let deadline = Instant::now() + Duration::from_secs(30);
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
            // Accepted sockets inherit the listener's nonblocking mode on macOS.
            stream.set_nonblocking(false)?;
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
            } else if path.ends_with("/ports") {
                "[]"
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
    #[cfg(unix)]
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
    #[cfg(unix)]
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
    jio_client::VmClient::with_state_directory(&endpoint, &key, &directory.0)?
        .set_current_session_id(&"e".repeat(32))?;
    let output = directory.command(&endpoint).arg("ports").output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"No public HTTP exposures.\n");
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("Local SSH forwards (jio forward) are not listed here")
    );
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

    #[cfg(unix)]
    {
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
    }
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
    #[cfg(unix)]
    {
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
    }
    Ok(())
}
