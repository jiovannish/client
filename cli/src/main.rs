mod app;
mod runner;

use std::env;
use std::io;
use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    match options().and_then(|(source, host, instances)| app::run(source, host, instances)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("jio: {error}");
            ExitCode::FAILURE
        }
    }
}

fn options() -> io::Result<(runner::Source, String, usize)> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some(std::ffi::OsStr::new("run")) {
        return Err(usage());
    }
    let source = arguments.next().ok_or_else(usage)?;
    let mut host = env::var("JIO_HOST").ok();
    let mut instances = 1;
    let mut language = None;

    while let Some(argument) = arguments.next() {
        if argument == "--host" {
            host = Some(
                arguments
                    .next()
                    .ok_or_else(usage)?
                    .into_string()
                    .map_err(|_| usage())?,
            );
        } else if argument == "--instances" {
            instances = arguments
                .next()
                .ok_or_else(usage)?
                .into_string()
                .map_err(|_| usage())?
                .parse()
                .map_err(|_| usage())?;
        } else if argument == "--language" {
            language = Some(
                arguments
                    .next()
                    .ok_or_else(usage)?
                    .into_string()
                    .map_err(|_| usage())?,
            );
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
    Ok((source, host, instances))
}

fn usage() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        "expected: jio run <main.rs|main.py> ... or jio run <code> --language python ...",
    )
}
