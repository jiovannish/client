use jio_client::{SessionClient, VmClient};
use std::{ffi::OsString, io};

pub const USAGE: &str = "usage: jio expose <port> [session-id] [--domain hostname] [--host host]\n       jio unexpose <port> [session-id] [--host host]\n       jio ports [session-id] [--host host]\n       jio domains add <hostname> <port> [session-id] [--host host]\n       jio domains status <hostname> [--host host]\n       jio domains remove <hostname> [--host host]\n\nexpose publishes HTTP, SSE and WebSockets only. For PostgreSQL or private TCP, use jio forward --help.";
pub const FORWARD_USAGE: &str = "usage: jio forward <port> [session-id] [--local-port port] [--host host]\n\nForward local 127.0.0.1:<local-port> to guest 127.0.0.1:<port> over authenticated SSH.\nThe local port defaults to the guest port; the session defaults to the current VM.\nKeep this command running; Ctrl-C closes the forward. Requires a guest template\npermitting local SSH forwarding (older templates reject connections).\n\nPostgreSQL: jio forward 5432 --local-port 15432\nThen: psql -h 127.0.0.1 -p 15432 -U <database-user> -d <database>\nDatabase authentication is still required. No public URL is created.";
pub struct Options {
    command: String,
    arguments: Vec<String>,
    host: String,
    domain: Option<String>,
    local_port: Option<u16>,
}
pub fn parse(
    command: &str,
    arguments: impl Iterator<Item = OsString>,
) -> io::Result<super::Invocation> {
    let mut positionals = Vec::new();
    let mut host = None;
    let mut domain = None;
    let mut local_port = None;
    let usage = if command == "forward" {
        FORWARD_USAGE
    } else {
        USAGE
    };
    let mut arguments = arguments.map(|a| a.into_string().map_err(|_| io::Error::other(usage)));
    while let Some(argument) = arguments.next() {
        let argument = argument?;
        match argument.as_str() {
            "--help" | "-h" => return Ok(super::Invocation::Help(usage)),
            "--host" if host.is_none() => {
                host = Some(arguments.next().ok_or_else(|| io::Error::other(usage))??)
            }
            "--domain" if command == "expose" && domain.is_none() => {
                domain = Some(arguments.next().ok_or_else(|| io::Error::other(USAGE))??)
            }
            "--local-port" if command == "forward" && local_port.is_none() => {
                local_port = Some(port(
                    &arguments.next().ok_or_else(|| io::Error::other(usage))??,
                )?);
            }
            s if s.starts_with('-') => return Err(io::Error::other(usage)),
            _ => positionals.push(argument),
        }
    }
    let count = positionals.len();
    let valid = match command {
        "expose" | "unexpose" | "forward" => count == 1 || count == 2,
        "ports" => count <= 1,
        "domains" => match positionals.first().map(String::as_str) {
            Some("add") => count == 3 || count == 4,
            Some("status" | "remove") => count == 2,
            _ => false,
        },
        _ => false,
    };
    if !valid {
        return Err(io::Error::other(usage));
    }
    if matches!(command, "expose" | "unexpose" | "forward") {
        port(&positionals[0])?;
    } else if command == "domains" && positionals[0] == "add" {
        port(&positionals[2])?;
    }
    if let Some(name) = &domain {
        jio_client::ingress::validate_domain(name)?;
    }
    if command == "domains" {
        jio_client::ingress::validate_domain(&positionals[1])?;
    }
    Ok(super::Invocation::Ingress(Options {
        command: command.into(),
        arguments: positionals,
        host: jio_client::resolve_endpoint(host)?,
        domain,
        local_port,
    }))
}
fn port(s: &str) -> io::Result<u16> {
    s.parse::<u16>().ok().filter(|p| *p > 0).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "port must be between 1 and 65535",
        )
    })
}
pub fn run(options: Options) -> io::Result<()> {
    let api_key = crate::config::api_key(&options.host)?;
    let api = SessionClient::new(&options.host, &api_key)?;
    let args = &options.arguments;
    match options.command.as_str() {
        "forward" => {
            let guest_port = port(&args[0])?;
            let local_port = options.local_port.unwrap_or(guest_port);
            let id =
                crate::session::target_session_id(&options.host, args.get(1).map(String::as_str))?;
            let vm = VmClient::new(&options.host, &api_key)?.attach(&id)?;
            forward(&vm, local_port, guest_port)?;
        }
        "expose" => {
            let port = port(&args[0])?;
            let id =
                crate::session::target_session_id(&options.host, args.get(1).map(String::as_str))?;
            let vm = VmClient::new(&options.host, &api_key)?.attach(&id)?;
            let exposure = if jio_client::ingress::supports_persistent_helper(&vm)? {
                api.expose_renewable(&id, port)?
            } else {
                api.expose(&id, port)?
            };
            if exposure.credential.is_some() {
                jio_client::ingress::install_helper(
                    &vm,
                    &options.host,
                    &exposure,
                    env!("CARGO_PKG_VERSION"),
                )?;
            }
            let mut connected = false;
            for _ in 0..10 {
                if api.ports(&id)?.iter().any(|p| {
                    p.port == port && p.generation == exposure.generation && p.status == "published"
                }) {
                    connected = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
            if !connected {
                return Err(io::Error::other(
                    "helper has not connected; inspect with jio ports and retry jio expose after 35 seconds",
                ));
            }
            println!("{}", exposure.url);
            if let Some(name) = options.domain {
                print_domain(api.add_domain(&name, &id, port)?);
            }
        }
        "unexpose" => {
            let id =
                crate::session::target_session_id(&options.host, args.get(1).map(String::as_str))?;
            api.unexpose(&id, port(&args[0])?)?;
            println!("Port {} unpublished.", args[0]);
        }
        "ports" => {
            let id =
                crate::session::target_session_id(&options.host, args.first().map(String::as_str))?;
            let ports = api.ports(&id)?;
            if ports.is_empty() {
                println!("No exposed ports.");
            }
            for p in ports {
                println!("{}\t{}\t{}", p.port, p.status, p.url);
            }
        }
        "domains" => match args[0].as_str() {
            "add" => {
                let id = crate::session::target_session_id(
                    &options.host,
                    args.get(3).map(String::as_str),
                )?;
                print_domain(api.add_domain(&args[1], &id, port(&args[2])?)?);
            }
            "status" => print_domain(api.domain_status(&args[1])?),
            "remove" => {
                api.remove_domain(&args[1])?;
                println!("Domain removed.");
            }
            _ => return Err(io::Error::other(USAGE)),
        },
        _ => return Err(io::Error::other(USAGE)),
    }
    Ok(())
}

fn forward(vm: &jio_client::Vm, local_port: u16, guest_port: u16) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _entered = runtime.enter();
    #[cfg(unix)]
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    #[cfg(unix)]
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let mut hangup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
    let mut forward = vm.forward(local_port, guest_port)?;
    eprintln!(
        "Opening 127.0.0.1:{local_port} -> guest 127.0.0.1:{guest_port}. Ctrl-C to stop.\nSSH reports rejected or unreachable guest connections below; the database must accept TCP connections."
    );
    runtime.block_on(async {
        let stopped = async {
            #[cfg(unix)]
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
                _ = hangup.recv() => {},
            }
            #[cfg(windows)]
            tokio::signal::ctrl_c().await?;
            Ok::<(), io::Error>(())
        };
        tokio::pin!(stopped);
        loop {
            tokio::select! {
                biased;
                result = &mut stopped => return result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => forward.check()?,
            }
        }
    })
}
fn print_domain(domain: jio_client::ingress::DomainStatus) {
    println!("{}  {}", domain.url, domain.status);
    println!("\nType  Name  Value");
    for record in domain.records {
        println!("{}  {}  {}", record.kind, record.name, record.value);
    }
    println!(
        "\nSubdomains can use CNAME {} instead of A.",
        domain.subdomain_cname_alternative
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forward_validates_ports_and_keeps_public_ingress_options_separate() -> io::Result<()> {
        for args in [
            vec![],
            vec!["0"],
            vec!["65536"],
            vec!["-1"],
            vec!["abc"],
            vec!["5432", "--local-port", "0"],
            vec!["5432", "--local-port", "65536"],
            vec!["5432", "--local-port"],
            vec!["5432", "--domain", "db.example.org"],
            vec!["5432", "--local-port", "5433", "--local-port", "5434"],
            vec!["5432", "a", "b"],
        ] {
            assert!(parse("forward", args.into_iter().map(OsString::from)).is_err());
        }
        let parsed = parse(
            "forward",
            ["5432", "--local-port", "15432", "--host", "ubuntu@test"]
                .into_iter()
                .map(OsString::from),
        )?;
        assert!(matches!(
            parsed,
            super::super::Invocation::Ingress(Options {
                local_port: Some(15432),
                ..
            })
        ));
        assert!(matches!(
            parse("forward", ["--help"].into_iter().map(OsString::from))?,
            super::super::Invocation::Help(FORWARD_USAGE)
        ));
        assert!(
            parse(
                "expose",
                ["3000", "--local-port", "3001"]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn parses_ports_and_rejects_ambiguous_arguments() {
        for value in ["0", "65536", "-1", "abc"] {
            assert!(port(value).is_err());
        }
        assert_eq!(port("3000").ok(), Some(3000));
        assert!(
            parse(
                "expose",
                ["3000", "--domain", "app.example.org"]
                    .into_iter()
                    .map(OsString::from)
            )
            .is_ok()
        );
        assert!(parse("ports", ["a", "b"].into_iter().map(OsString::from)).is_err());
        assert!(
            parse(
                "domains",
                ["add", "example.org"].into_iter().map(OsString::from)
            )
            .is_err()
        );
    }
}
