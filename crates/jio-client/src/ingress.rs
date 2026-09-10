//! Opt-in HTTP publication through a detached, loopback-only guest helper.
use super::*;
use serde_json::{Value, json};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::sync::Arc;
use std::{io::Write, path::Path};
use tokio::io::AsyncWriteExt;

/// Published port; the one-time helper credential is intentionally not Debug.
#[derive(Deserialize)]
pub struct PortExposure {
    pub session_id: String,
    pub port: u16,
    pub generation: u64,
    pub url: String,
    pub status: String,
    pub credential: Option<String>,
    pub expires_at: Option<u64>,
    pub ttl_seconds: Option<u64>,
}
/// DNS ownership instructions and the observed state of a custom domain.
#[derive(Debug, Deserialize)]
pub struct DomainStatus {
    pub hostname: String,
    pub url: String,
    pub status: String,
    pub records: Vec<DnsRecord>,
    pub subdomain_cname_alternative: String,
}
/// One DNS record to copy into the domain provider.
#[derive(Debug, Deserialize)]
pub struct DnsRecord {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub value: String,
}
impl SessionClient {
    fn ingress_request(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> io::Result<Value> {
        let connection = Connection::open(&self.endpoint, &self.api_key, Duration::from_secs(30))?;
        let mut request = connection
            .client
            .request(method, format!("{}{path}", connection.base_url))
            .bearer_auth(&self.api_key);
        if let Some(body) = body {
            request = request
                .header(CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(&body).map_err(invalid)?);
        }
        let response = request.send().map_err(other)?;
        if response.status() == StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        decode(response)
    }
    /// Publish an HTTP port. Repeating a healthy publication returns no new credential.
    pub fn expose(&self, id: &str, port: u16) -> io::Result<PortExposure> {
        target(id, port)?;
        let exposure: PortExposure = serde_json::from_value(self.ingress_request(
            reqwest::Method::POST,
            &format!("/v1/sessions/{id}/ports/{port}"),
            None,
        )?)
        .map_err(invalid)?;
        if exposure.session_id != id || exposure.port != port || exposure.generation == 0 {
            return Err(invalid("endpoint returned a different exposure"));
        }
        Ok(exposure)
    }
    /// Revoke publication and its helper capability.
    pub fn unexpose(&self, id: &str, port: u16) -> io::Result<()> {
        target(id, port)?;
        self.ingress_request(
            reqwest::Method::DELETE,
            &format!("/v1/sessions/{id}/ports/{port}"),
            None,
        )?;
        Ok(())
    }
    /// List published ports and their current connection status.
    pub fn ports(&self, id: &str) -> io::Result<Vec<PortExposure>> {
        target(id, 1)?;
        serde_json::from_value(self.ingress_request(
            reqwest::Method::GET,
            &format!("/v1/sessions/{id}/ports"),
            None,
        )?)
        .map_err(invalid)
    }
    /// Claim a custom hostname and return the DNS records required to verify it.
    pub fn add_domain(&self, name: &str, id: &str, port: u16) -> io::Result<DomainStatus> {
        validate_domain(name)?;
        target(id, port)?;
        serde_json::from_value(self.ingress_request(
            reqwest::Method::POST,
            "/v1/domains",
            Some(json!({"hostname":name,"session_id":id,"port":port})),
        )?)
        .map_err(invalid)
    }
    /// Check DNS ownership and certificate status.
    pub fn domain_status(&self, name: &str) -> io::Result<DomainStatus> {
        validate_domain(name)?;
        serde_json::from_value(self.ingress_request(
            reqwest::Method::GET,
            &format!("/v1/domains/{name}"),
            None,
        )?)
        .map_err(invalid)
    }
    /// Remove a hostname alias. The default URL remains available.
    pub fn remove_domain(&self, name: &str) -> io::Result<()> {
        validate_domain(name)?;
        self.ingress_request(
            reqwest::Method::DELETE,
            &format!("/v1/domains/{name}"),
            None,
        )?;
        Ok(())
    }
}
fn target(id: &str, port: u16) -> io::Result<()> {
    if id.len() != 32
        || !id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || port == 0
    {
        return Err(invalid("invalid session ID or port"));
    }
    Ok(())
}
/// Validate a DNS hostname before performing publication side effects.
pub fn validate_domain(name: &str) -> io::Result<()> {
    if name.len() > 253
        || !name.contains('.')
        || name.split('.').any(|s| {
            s.is_empty()
                || s.len() > 63
                || s.starts_with('-')
                || s.ends_with('-')
                || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
    {
        return Err(invalid("invalid DNS hostname"));
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Helper {
    endpoint: String,
    session_id: String,
    port: u16,
    credential: String,
    ttl_seconds: u64,
}
impl Helper {
    fn validate(&self) -> io::Result<()> {
        target(&self.session_id, self.port)?;
        let url = reqwest::Url::parse(&self.endpoint).map_err(invalid)?;
        if (url.scheme() != "https"
            && !(url.scheme() == "http" && matches!(url.host_str(), Some("127.0.0.1" | "[::1]"))))
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
            || self.credential.len() != 64
            || !self.credential.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(invalid("invalid helper authority"));
        }
        if self.ttl_seconds == 0 || self.ttl_seconds > 14400 {
            return Err(invalid("unbounded helper lifetime"));
        }
        Ok(())
    }
}
/// Internal CLI mode. Reads a private capability from stdin, never an account key.
#[doc(hidden)]
pub fn run_helper() -> io::Result<()> {
    let mut bytes = Vec::new();
    io::stdin().take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(invalid("helper configuration too large"));
    }
    let config: Helper = serde_json::from_slice(&bytes).map_err(invalid)?;
    config.validate()?;
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(helper(config))
}
async fn helper(config: Helper) -> io::Result<()> {
    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .http1_only()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(5));
    // Never inherit the local operator's custom CA path into the guest.
    if let Some(ca) = connection_ca(&config.endpoint, None)? {
        builder = builder.add_root_certificate(ca);
    }
    let client = builder.build().map_err(other)?;
    let config = Arc::new(config);
    let standby = Arc::new(tokio::sync::Semaphore::new(4));
    let active = Arc::new(tokio::sync::Semaphore::new(16));
    // Guest wall clocks can differ after restore; only Server decides absolute expiry.
    let lifetime = Duration::from_secs(config.ttl_seconds);
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::channel::<()>(1);
    let run = async {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            // Finished task results contain no user data or capabilities.
            while tasks.try_join_next().is_some() {}
            let pending = standby.clone().acquire_owned().await.map_err(other)?;
            let current = active.clone().acquire_owned().await.map_err(other)?;
            let config = config.clone();
            let client = client.clone();
            let stop = stop_tx.clone();
            tasks.spawn(async move {
                let _current=current;let _pending=pending;
                let response=client.get(format!("{}/v1/sessions/{}/ports/{}/connect",config.endpoint.trim_end_matches('/'),config.session_id,config.port)).bearer_auth(&config.credential).header("connection","upgrade").header("upgrade","jio-ingress").timeout(Duration::from_secs(35)).send().await;
                let Ok(response)=response else {tokio::time::sleep(Duration::from_secs(2)).await;return};
                if matches!(response.status().as_u16(),401|403|404|410) {let _=stop.try_send(());return;}
                if response.status()!=StatusCode::SWITCHING_PROTOCOLS || response.headers().get("upgrade").is_none_or(|v| v != "jio-ingress") {
                    if response.status()!=StatusCode::NO_CONTENT {tokio::time::sleep(Duration::from_secs(2)).await;}
                    return;
                }
                drop(_pending); // A request has claimed this connection; replenish its standby slot.
                let Ok(mut tunnel)=response.upgrade().await else {return};
                let local=tokio::time::timeout(Duration::from_secs(5),async {
                    match tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST,config.port)).await {
                        Ok(stream)=>Ok(stream),
                        Err(_)=>tokio::net::TcpStream::connect((std::net::Ipv6Addr::LOCALHOST,config.port)).await,
                    }
                }).await;
                let Ok(Ok(mut local))=local else {let _=tunnel.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;return};
                let _=local.set_nodelay(true);
                let _=tokio::time::timeout(Duration::from_secs(3600),tokio::io::copy_bidirectional(&mut tunnel,&mut local)).await;
            });
        }
        #[allow(unreachable_code)]
        Ok::<(), io::Error>(())
    };
    tokio::select! {result=run=>result,_=tokio::time::sleep(lifetime)=>Ok(()),_=stop_rx.recv()=>Ok(())}
}

/// Install this CLI release's Linux helper through an existing authenticated SSH session.
/// `JIO_INGRESS_BINARY` can select a locally built Linux binary during development.
pub fn install_helper(
    vm: &Vm,
    endpoint: &str,
    exposure: &PortExposure,
    version: &str,
) -> io::Result<()> {
    if vm.session().session_id != exposure.session_id
        || vm.session().generation != exposure.generation
    {
        return Err(invalid(
            "VM changed before helper installation; republish the port",
        ));
    }
    let Some(credential) = &exposure.credential else {
        return Ok(());
    };
    let config = Helper {
        endpoint: endpoint.into(),
        session_id: exposure.session_id.clone(),
        port: exposure.port,
        credential: credential.clone(),
        ttl_seconds: exposure
            .ttl_seconds
            .ok_or_else(|| invalid("missing helper lifetime"))?,
    };
    config.validate()?;
    // ponytail: deployed snapshots leave lo down; remove this compatibility setup
    // once every supported template initializes loopback before session readiness.
    let arch = vm.exec(
        "set -eu; if ! ip -o link show dev lo | grep -Eq '(<|,)UP(,|>)'; then sudo -n ip link set dev lo up; fi; uname -m",
        Duration::from_secs(10),
    )?;
    if !arch.success() {
        return Err(io::Error::other(
            "cannot prepare guest loopback or determine architecture; bring lo up before exposing a port",
        ));
    }
    let target = match std::str::from_utf8(&arch.stdout).map_err(invalid)?.trim() {
        "x86_64" => "x86_64-unknown-linux-gnu",
        "aarch64" => "aarch64-unknown-linux-gnu",
        _ => return Err(invalid("unsupported guest architecture")),
    };
    let binary = helper_binary(version, target)?;
    let data = serde_json::to_vec(&config).map_err(invalid)?;
    if data.len() > 4096 {
        return Err(invalid("helper configuration too large"));
    }
    let command = install_command(binary.len(), version)?;
    let mut payload = binary;
    payload.extend_from_slice(&data);
    let installed = vm.exec_with_input(&command, Some(&payload), Duration::from_secs(120))?;
    if !installed.success() {
        return Err(io::Error::other(
            "helper transfer, version verification or startup failed",
        ));
    }
    Ok(())
}

fn install_command(binary_bytes: usize, version: &str) -> io::Result<String> {
    validate_helper_version(version)?;
    Ok(format!(
        "set -- {binary_bytes} 'jio {version}'\n{INSTALL_HELPER}"
    ))
}

// The receiving shell owns cleanup until the detached helper acknowledges ownership.
// EOF, version failures and catchable signals clean up without another SSH connection.
const INSTALL_HELPER: &str = r#"set -eu
umask 077
root=$(mktemp -d "${TMPDIR:-/tmp}/jio-ingress.XXXXXXXXXXXX")
trap 'rm -rf -- "$root"' 0
trap 'exit 1' 1 2 15
cat > "$root/payload"
head -c "$1" "$root/payload" > "$root/jio"
[ "$(wc -c < "$root/jio")" -eq "$1" ]
chmod 700 "$root/jio"
[ "$("$root/jio" --version)" = "$2" ]
tail -c +"$(($1 + 1))" "$root/payload" > "$root/config"
rm "$root/payload"
command -v nohup >/dev/null
nohup sh -c '
    set -eu
    root=$1
    trap '\''rm -rf -- "$root"'\'' 0
    trap '\''exit 1'\'' 2 15
    touch "$root/owned"
    "$root/jio" __ingress-helper < "$root/config"
' sh "$root" </dev/null >/dev/null 2>&1 &
helper_pid=$!
attempt=0
until [ -f "$root/owned" ]; do
    kill -0 "$helper_pid"
    [ "$attempt" -lt 100 ]
    attempt=$((attempt + 1))
    sleep 0.05
done
trap - 0 1 2 15
"#;

fn validate_helper_version(version: &str) -> io::Result<()> {
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err(invalid("invalid helper release version"));
    }
    Ok(())
}

fn helper_binary(version: &str, target: &str) -> io::Result<Vec<u8>> {
    if let Some(path) = env::var_os("JIO_INGRESS_BINARY") {
        return read_binary(Path::new(&path));
    }
    validate_helper_version(version)?;
    let root = env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| invalid("HOME is required"))?
        .join(".cache/jio/helpers");
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)?;
    super::vm::require_directory(&root)?;
    let cached = root.join(format!("{version}-{target}"));
    if cached.exists() {
        super::vm::require_private_file(&cached)?;
        return read_binary(&cached);
    }
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::limited(5))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(other)?;
    let base = format!("https://github.com/jiovannish/client/releases/download/v{version}");
    let archive_name = format!("jio-{target}.tar.gz");
    let fetch = |name: &str, max: u64| -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        client
            .get(format!("{base}/{name}"))
            .send()
            .map_err(other)?
            .error_for_status()
            .map_err(other)?
            .take(max + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > max {
            return Err(invalid("helper release exceeds size limit"));
        }
        Ok(bytes)
    };
    let sums = fetch("SHA256SUMS", 16384)?;
    let sums = std::str::from_utf8(&sums).map_err(invalid)?;
    let expected = sums
        .lines()
        .find_map(|l| {
            let mut parts = l.split_whitespace();
            let digest = parts.next()?;
            let file = parts.next()?.trim_start_matches('*');
            (file == archive_name).then_some(digest)
        })
        .ok_or_else(|| invalid("release is missing helper checksum"))?;
    let archive = fetch(&archive_name, 32 * 1024 * 1024)?;
    if format!("{:x}", Sha256::digest(&archive)) != expected {
        return Err(invalid("helper checksum mismatch"));
    }
    let temporary = root.join(format!(".{version}-{target}-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(&archive)?;
    drop(file);
    // Extract only the named regular binary, never arbitrary archive paths.
    let output = std::process::Command::new("tar")
        .args(["-xOzf"])
        .arg(&temporary)
        .arg("jio")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let extracted = (|| {
        let mut child = output?;
        let mut bytes = Vec::new();
        child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing tar output"))?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 * 1024 {
            let _ = child.kill();
            let _ = child.wait();
            return Err(invalid("helper binary exceeds size limit"));
        }
        if !child.wait()?.success() || !bytes.starts_with(b"\x7fELF") {
            return Err(invalid("invalid Linux helper archive"));
        }
        Ok(bytes)
    })();
    let _ = std::fs::remove_file(&temporary);
    let bytes = extracted?;
    let staging = super::vm::temporary_directory(&root)?;
    let result = (|| {
        super::vm::write_private_file(&staging.join("jio"), &bytes)?;
        std::fs::rename(staging.join("jio"), &cached)
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result?;
    Ok(bytes)
}
fn read_binary(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(16 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16 * 1024 * 1024 || !bytes.starts_with(b"\x7fELF") {
        return Err(invalid("expected a Linux helper binary up to 16 MiB"));
    }
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helper_install_cleans_failed_transfers_and_hands_off_cleanup() -> io::Result<()> {
        use std::process::{Command, Stdio};
        let temporary = super::super::vm::temporary_directory(&env::temp_dir())?;
        let binary = b"#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'jio 0.2.0'; exit; fi\ncat >/dev/null\nwhile [ ! -f \"$TMPDIR/finish\" ]; do sleep 0.05; done\nexit 7\n";
        let run = |version: &str, bytes: &[u8]| -> io::Result<std::process::Output> {
            let mut child = Command::new("sh")
                .args(["-c", &install_command(binary.len(), version)?])
                .env("TMPDIR", &temporary)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            child
                .stdin
                .take()
                .ok_or_else(|| io::Error::other("missing test stdin"))?
                .write_all(bytes)?;
            child.wait_with_output()
        };
        // A truncated stream and a full binary with the wrong version both remove the directory.
        for (version, bytes) in [("0.2.0", &binary[..10]), ("0.0.0", &binary[..])] {
            assert!(!run(version, bytes)?.status.success());
            assert_eq!(std::fs::read_dir(&temporary)?.count(), 0);
        }
        let mut payload = binary.to_vec();
        payload.extend_from_slice(b"{}\n");
        let started = run("0.2.0", &payload)?;
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        assert_eq!(std::fs::read_dir(&temporary)?.count(), 1);
        let directory = std::fs::read_dir(&temporary)?
            .next()
            .ok_or_else(|| io::Error::other("missing helper directory"))??
            .path();
        assert_eq!(std::fs::read(directory.join("jio"))?, binary);
        assert_eq!(std::fs::read(directory.join("config"))?, b"{}\n");
        // Exiting the installing shell leaves the helper alive; its own failure cleans up later.
        std::fs::write(temporary.join("finish"), b"")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_dir(&temporary)?.count() != 1 {
            if Instant::now() >= deadline {
                return Err(io::Error::other("helper did not clean up after exit"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        std::fs::remove_dir_all(temporary)?;
        assert!(install_command(1, "0.2.0'; exit 0").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn helper_falls_back_to_ipv6_loopback() -> io::Result<()> {
        use tokio::io::AsyncReadExt;
        let app = tokio::net::TcpListener::bind("[::1]:0").await?;
        let control = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let config = Helper {
            endpoint: format!("http://{}", control.local_addr()?),
            session_id: "a".repeat(32),
            port: app.local_addr()?.port(),
            credential: "b".repeat(64),
            ttl_seconds: 60,
        };
        let task = tokio::spawn(helper(config));
        let result=tokio::time::timeout(Duration::from_secs(5),async {
            let (mut upstream,_)=control.accept().await?;
            let mut header=Vec::new(); let mut byte=[0;1];
            while !header.ends_with(b"\r\n\r\n") {
                upstream.read_exact(&mut byte).await?;header.push(byte[0]);
                if header.len()>4096 {return Err(invalid("test header exceeded bound"));}
            }
            upstream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: jio-ingress\r\n\r\nhello").await?;
            let (mut local,_)=app.accept().await?;let mut message=[0;5];
            local.read_exact(&mut message).await?;assert_eq!(&message,b"hello");
            local.write_all(b"world").await?;upstream.read_exact(&mut message).await?;
            assert_eq!(&message,b"world");Ok::<_,io::Error>(())
        }).await.map_err(other)?;
        task.abort();
        result
    }
    #[test]
    fn rejects_helper_redirects_credentials_and_unbounded_leases() {
        let mut helper = Helper {
            endpoint: "http://127.0.0.1:8080".into(),
            session_id: "a".repeat(32),
            port: 3000,
            credential: "b".repeat(64),
            ttl_seconds: 3600,
        };
        assert!(helper.validate().is_ok());
        for endpoint in [
            "http://example.org",
            "https://user:pass@example.org",
            "https://example.org/path",
            "https://example.org?token=x",
        ] {
            helper.endpoint = endpoint.into();
            assert!(helper.validate().is_err());
        }
        helper.endpoint = "https://example.org".into();
        helper.ttl_seconds = u64::MAX;
        assert!(helper.validate().is_err());
        assert!(target(&"a".repeat(32), 0).is_err());
        assert!(validate_domain("../evil").is_err());
    }
}
