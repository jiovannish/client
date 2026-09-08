//! One owned HTTPS/SSH connection per Vm handle, not a global client daemon.
use super::*;

pub(super) struct Gateway {
    pub socket: PathBuf,
    pub port: u16,
    process: Option<Child>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    relay: Option<thread::JoinHandle<()>>,
    directory: PathBuf,
}

impl Gateway {
    pub fn open(
        owner: &VmClient,
        session: &Session,
        credentials: &Credentials,
    ) -> io::Result<Self> {
        // Short, private directory avoids Unix socket path limits on macOS.
        let directory = temporary_directory(Path::new("/tmp"))?;
        let mut gateway = Self {
            socket: directory.join("ssh"),
            port: 0,
            directory,
            process: None,
            cancel: None,
            relay: None,
        };
        let ca = crate::custom_ca(owner.api.endpoint())?;
        let endpoint = format!(
            "{}/v0/sessions/{}/ssh",
            owner.api.endpoint(),
            session.session_id
        );
        let key = owner.api.api_key.clone();
        let generation = session.generation;
        let (ready, receiver) = std::sync::mpsc::sync_channel(1);
        let (cancel, stopped) = tokio::sync::oneshot::channel();
        gateway.cancel = Some(cancel);
        gateway.relay = Some(thread::spawn(move || {
            let result = (|| -> io::Result<()> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    let mut builder = reqwest::Client::builder()
                        .no_proxy()
                        .http1_only()
                        .redirect(reqwest::redirect::Policy::none())
                        .connect_timeout(Duration::from_secs(5));
                    if let Some(ca) = ca {
                        builder = builder.add_root_certificate(ca);
                    }
                    let client = builder.build().map_err(crate::other)?;
                    let response = client
                        .get(endpoint)
                        .bearer_auth(key)
                        .header("connection", "upgrade")
                        .header("upgrade", "jio-ssh")
                        .header("x-jio-generation", generation)
                        .timeout(Duration::from_secs(10))
                        .send()
                        .await
                        .map_err(crate::other)?;
                    if response.status() != reqwest::StatusCode::SWITCHING_PROTOCOLS {
                        return Err(io::Error::other(format!(
                            "SSH gateway returned {}",
                            response.status()
                        )));
                    }
                    let mut remote = response.upgrade().await.map_err(crate::other)?;
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
                    ready
                        .send(Ok(listener.local_addr()?.port()))
                        .map_err(crate::other)?;
                    let copy = async {
                        let (mut local, _) = listener.accept().await?;
                        local.set_nodelay(true)?;
                        drop(listener); // Exactly one SSH master; no local open proxy.
                        tokio::io::copy_bidirectional(&mut local, &mut remote).await?;
                        Ok::<(), io::Error>(())
                    };
                    tokio::select! { result = copy => result, _ = stopped => Ok(()) }
                })
            })();
            if let Err(error) = result {
                let _ = ready.send(Err(error));
            }
        }));
        gateway.port = receiver
            .recv_timeout(Duration::from_secs(15))
            .map_err(crate::other)??;
        let mut command = owner.ssh_command_at(
            session,
            credentials,
            TtyMode::Disabled,
            Some((&gateway.socket, gateway.port, true)),
        )?;
        gateway.process = Some(
            command
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()?,
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while !gateway.socket.try_exists()? {
            gateway.check()?;
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "SSH master did not become ready",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(gateway)
    }

    pub fn check(&mut self) -> io::Result<()> {
        let child = self
            .process
            .as_mut()
            .ok_or_else(|| io::Error::other("SSH master missing"))?;
        if child.try_wait()?.is_some() || self.relay.as_ref().is_some_and(|t| t.is_finished()) {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "SSH connection closed; attach again (commands are never replayed)",
            ));
        }
        Ok(())
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if let Some(child) = &mut self.process {
            terminate(child);
        }
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }
        if let Some(relay) = self.relay.take() {
            let _ = relay.join();
        }
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_dir(&self.directory);
    }
}
