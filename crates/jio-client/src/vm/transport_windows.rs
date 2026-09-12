//! Windows OpenSSH has no ControlMaster: relay one HTTPS upgrade per SSH connection.
use super::*;

pub(super) struct Gateway {
    pub socket: PathBuf,
    pub port: u16,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    relay: Option<thread::JoinHandle<()>>,
}

impl Gateway {
    pub fn open(owner: &VmClient, session: &Session, _: &Credentials) -> io::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        listener.set_nonblocking(true)?;
        let ca = crate::custom_ca(owner.api.endpoint())?;
        let endpoint = format!(
            "{}/v0/sessions/{}/ssh",
            owner.api.endpoint(),
            session.session_id
        );
        let key = owner.api.api_key.clone();
        let generation = session.generation;
        let (cancel, mut stopped) = tokio::sync::oneshot::channel();
        let (ready, initialized) = std::sync::mpsc::sync_channel(1);
        let relay = thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let listener = tokio::net::TcpListener::from_std(listener)?;
                    let mut builder = reqwest::Client::builder().no_proxy().http1_only()
                        .redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(5));
                    if let Some(ca) = ca { builder = builder.add_root_certificate(ca); }
                    let client = builder.build().map_err(crate::other)?;
                    ready.send(Ok(())).map_err(crate::other)?;
                    let mut connections = tokio::task::JoinSet::new();
                    loop {
                        tokio::select! {
                            _ = &mut stopped => break,
                            _ = connections.join_next(), if !connections.is_empty() => {},
                            accepted = listener.accept(), if connections.len() < 16 => {
                                let (mut local, _) = accepted?;
                                local.set_nodelay(true)?;
                                let client = client.clone();
                                let endpoint = endpoint.clone();
                                let key = key.clone();
                                connections.spawn(async move {
                                    let response = client.get(endpoint).bearer_auth(key)
                                        .header("connection", "upgrade").header("upgrade", "jio-ssh")
                                        .header("x-jio-generation", generation)
                                        .timeout(Duration::from_secs(10)).send().await.map_err(crate::other)?;
                                    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS
                                        || response.headers().get("upgrade").is_none_or(|v| v != "jio-ssh") {
                                        return Err(io::Error::other("SSH gateway rejected the connection"));
                                    }
                                    let mut remote = response.upgrade().await.map_err(crate::other)?;
                                    tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
                                    Ok::<(), io::Error>(())
                                });
                            }
                        }
                    }
                    Ok(()) // Dropping JoinSet aborts all outstanding connections.
                })
            })();
            if let Err(error) = result {
                let _ = ready.send(Err(error));
            }
        });
        let gateway = Self {
            socket: PathBuf::new(),
            port,
            cancel: Some(cancel),
            relay: Some(relay),
        };
        initialized
            .recv_timeout(Duration::from_secs(15))
            .map_err(crate::other)??;
        Ok(gateway)
    }

    pub fn check(&mut self) -> io::Result<()> {
        if self
            .relay
            .as_ref()
            .is_none_or(|thread| thread.is_finished())
        {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SSH connection closed; attach again",
            ));
        }
        Ok(())
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
    }
}

#[cfg(test)]
#[test]
fn independent_connections_are_scoped_and_close_with_the_gateway() -> io::Result<()> {
    use std::io::{Read, Write};
    let server = std::net::TcpListener::bind("127.0.0.1:0")?;
    let owner = VmClient {
        api: SessionClient::new(format!("http://{}", server.local_addr()?), "a".repeat(64))?,
        sessions: PathBuf::new(),
    };
    let session = super::tests::ready_session();
    let id = session.session_id.clone();
    let generation = session.generation;
    let upstream = thread::spawn(move || -> io::Result<()> {
        let mut streams = Vec::new();
        for _ in 0..2 {
            let (mut stream, _) = server.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte)?;
                request.push(byte[0]);
                if request.len() > 4096 {
                    return Err(io::Error::other("oversized test header"));
                }
            }
            let request = String::from_utf8(request)
                .map_err(io::Error::other)?
                .to_lowercase();
            assert!(request.starts_with(&format!("get /v0/sessions/{id}/ssh ")));
            assert!(request.contains(&format!("x-jio-generation: {generation}")));
            assert!(request.contains(&format!("authorization: bearer {}", "a".repeat(64))));
            stream.write_all(b"HTTP/1.1 101 Switching Protocols\r\nConnection: upgrade\r\nUpgrade: jio-ssh\r\n\r\nready")?;
            let mut bytes = [0; 4];
            stream.read_exact(&mut bytes)?;
            assert_eq!(&bytes, b"ping");
            stream.write_all(b"pong")?;
            streams.push(stream);
        }
        for mut stream in streams {
            assert_eq!(stream.read(&mut [0])?, 0);
        }
        Ok(())
    });
    let credentials = Credentials {
        private_key: PathBuf::new(),
        known_hosts: PathBuf::new(),
    };
    let mut gateway = Gateway::open(&owner, &session, &credentials)?;
    gateway.check()?;
    assert!(gateway.socket.as_os_str().is_empty());
    let mut streams = Vec::new();
    for _ in 0..2 {
        let mut stream =
            std::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, gateway.port))?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        let mut ready = [0; 5];
        stream.read_exact(&mut ready)?;
        assert_eq!(&ready, b"ready");
        stream.write_all(b"ping")?;
        let mut reply = [0; 4];
        stream.read_exact(&mut reply)?;
        assert_eq!(&reply, b"pong");
        streams.push(stream);
    }
    drop(gateway);
    for mut stream in streams {
        assert_eq!(stream.read(&mut [0])?, 0);
    }
    upstream
        .join()
        .map_err(|_| io::Error::other("upstream test failed"))?
}
