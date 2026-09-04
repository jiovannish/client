mod app;
mod runner;
mod session;

use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

enum Invocation {
    Run {
        source: runner::Source,
        host: String,
        instances: usize,
        concurrency: usize,
    },
    Create {
        host: String,
    },
    Connect {
        host: String,
        id: String,
    },
    Destroy {
        host: String,
        id: String,
    },
}

fn main() -> ExitCode {
    match options().and_then(execute) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("jio: {error}");
            ExitCode::FAILURE
        }
    }
}

fn execute(invocation: Invocation) -> io::Result<()> {
    match invocation {
        Invocation::Run {
            source,
            host,
            instances,
            concurrency,
        } => app::run(source, host, instances, concurrency),
        Invocation::Create { host } => {
            let created = session::create(host)?;
            println!("{}", created.session_id);
            Ok(())
        }
        Invocation::Connect { host, id } => session::connect(host, &id),
        Invocation::Destroy { host, id } => session::destroy(host, &id),
    }
}

fn options() -> io::Result<Invocation> {
    options_from(env::args_os().skip(1))
}

fn options_from(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    match arguments.next().as_deref() {
        Some(command) if command == OsStr::new("run") => run_options(arguments),
        Some(command) if command == OsStr::new("create") => create_options(arguments),
        Some(command) if command == OsStr::new("connect") || command == OsStr::new("attach") => {
            session_options(arguments, false)
        }
        Some(command) if command == OsStr::new("destroy") => session_options(arguments, true),
        _ => Err(usage()),
    }
}

fn run_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let source = arguments.next().ok_or_else(usage)?;
    let mut host = env::var("JIO_HOST").ok();
    let mut instances = 1;
    let mut concurrency = None;
    let mut language = None;

    while let Some(argument) = arguments.next() {
        if argument == "--host" {
            host = Some(text(arguments.next(), "host")?);
        } else if argument == "--instances" {
            instances = text(arguments.next(), "instance count")?
                .parse()
                .map_err(|_| usage())?;
        } else if argument == "--concurrency" {
            concurrency = Some(
                text(arguments.next(), "concurrency")?
                    .parse()
                    .map_err(|_| usage())?,
            );
        } else if argument == "--language" {
            language = Some(text(arguments.next(), "language")?);
        } else {
            return Err(usage());
        }
    }

    let host = host.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing --host <user@address|https://url> (or JIO_HOST)",
        )
    })?;
    if !(1..=jio_client::MAX_INSTANCES).contains(&instances) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "instance count must be between 1 and {}",
                jio_client::MAX_INSTANCES
            ),
        ));
    }
    let concurrency = concurrency.unwrap_or(instances);
    if !(1..=instances).contains(&concurrency) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "concurrency must be between 1 and the instance count",
        ));
    }
    let source = match language.as_deref() {
        None => runner::Source::File(PathBuf::from(source)),
        Some("python") => runner::Source::Python(source.into_string().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Python source is not UTF-8")
        })?),
        Some(language) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("unsupported language: {language}"),
            ));
        }
    };
    Ok(Invocation::Run {
        source,
        host,
        instances,
        concurrency,
    })
}

fn create_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let mut host = None;
    while let Some(argument) = arguments.next() {
        if argument == "--host" {
            if host.is_some() {
                return Err(usage());
            }
            host = Some(arguments.next().ok_or_else(usage)?);
        } else {
            return Err(usage());
        }
    }
    Ok(Invocation::Create {
        host: session::host(host)?,
    })
}

fn session_options(
    mut arguments: impl Iterator<Item = OsString>,
    destroy: bool,
) -> io::Result<Invocation> {
    let id = text(arguments.next(), "session ID")?;
    if !jio_client::valid_session_id(&id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session ID is invalid",
        ));
    }
    let mut host = None;
    while let Some(argument) = arguments.next() {
        if argument != "--host" || host.is_some() {
            return Err(usage());
        }
        host = Some(arguments.next().ok_or_else(usage)?);
    }
    let host = session::host(host)?;
    if destroy {
        Ok(Invocation::Destroy { host, id })
    } else {
        Ok(Invocation::Connect { host, id })
    }
}

fn text(value: Option<OsString>, name: &str) -> io::Result<String> {
    value
        .ok_or_else(usage)?
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is not UTF-8")))
}

fn usage() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected: jio run <source> [options] | jio create [--host <host>] | jio connect|attach <session-id> [--host <host>] | jio destroy <session-id> [--host <host>]",
    )
}

#[cfg(test)]
mod tests {
    use super::{Invocation, options_from};
    use std::ffi::OsString;

    fn arguments(command: &str) -> impl Iterator<Item = OsString> {
        [
            command,
            "abababababababababababababababab",
            "--host",
            "ubuntu@host",
        ]
        .into_iter()
        .map(OsString::from)
    }

    #[test]
    fn connect_and_attach_address_the_same_existing_session() {
        for command in ["connect", "attach"] {
            let invocation = options_from(arguments(command));
            assert!(matches!(invocation, Ok(Invocation::Connect { .. })));
        }
    }
}
