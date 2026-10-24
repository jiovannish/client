use jio_client::{SessionClient, jobs::JobPlan};
use std::{
    ffi::OsString,
    fs::File,
    io::{self, Read, Write},
};

pub const USAGE: &str = "usage: jio job submit <plan.json> <idempotency-key> [--host host]\n       jio job status <job-id> [--host host]\n       jio job run <job-id> <step-id> <epoch> [--host host]\n       jio job cancel <job-id> [--host host]\n       jio job bind <job-id> <session-id> <generation> <epoch> [--host host]\n\nReturns JSON progress and durable results. Jobs use an existing owned session.\nEach step runs once; uncertain outcomes require reconciliation, never blind replay.\nBind supports the same session after retained restart at a completed step boundary.";

pub struct Options {
    host: String,
    command: String,
    args: Vec<String>,
}

pub fn parse(arguments: impl Iterator<Item = OsString>) -> io::Result<super::Invocation> {
    let mut host = None;
    let mut args = Vec::new();
    let mut arguments = arguments.map(|a| a.into_string().map_err(|_| invalid()));
    while let Some(arg) = arguments.next() {
        match arg?.as_str() {
            "--help" | "-h" => return Ok(super::Invocation::Help(USAGE)),
            "--host" if host.is_none() => host = Some(arguments.next().ok_or_else(invalid)??),
            s if s.starts_with('-') => return Err(invalid()),
            s => args.push(s.to_owned()),
        }
    }
    let command = args.first().ok_or_else(invalid)?.clone();
    args.remove(0);
    let valid = match command.as_str() {
        "submit" => args.len() == 2,
        "status" | "cancel" => args.len() == 1,
        "run" => args.len() == 3,
        "bind" => args.len() == 4,
        _ => false,
    };
    if !valid {
        return Err(invalid());
    }
    if command != "submit" && !jio_client::valid_session_id(&args[0]) {
        return Err(invalid());
    }
    if command == "run" {
        number(&args[2])?;
    }
    if command == "bind" {
        if !jio_client::valid_session_id(&args[1]) {
            return Err(invalid());
        }
        number(&args[2])?;
        number(&args[3])?;
    }
    Ok(super::Invocation::Job(Options {
        host: jio_client::resolve_endpoint(host)?,
        command,
        args,
    }))
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, USAGE)
}
fn number(s: &str) -> io::Result<u64> {
    s.parse().ok().filter(|n| *n > 0).ok_or_else(invalid)
}

pub fn run(options: Options) -> io::Result<()> {
    let key = crate::config::api_key(&options.host)?;
    let api = SessionClient::new(options.host, key)?;
    let a = &options.args;
    let job = match options.command.as_str() {
        "submit" => {
            let mut bytes = Vec::new();
            File::open(&a[0])?.take(32769).read_to_end(&mut bytes)?;
            if bytes.len() > 32768 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "job plan exceeds 32 KiB",
                ));
            }
            let plan: JobPlan = serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
            api.create_job(&a[1], &plan)?
        }
        "status" => api.job(&a[0])?,
        "run" => api.run_job_step(&a[0], &a[1], number(&a[2])?)?,
        "cancel" => api.cancel_job(&a[0])?,
        "bind" => api.bind_job(&a[0], number(&a[3])?, &a[1], number(&a[2])?)?,
        _ => return Err(invalid()),
    };
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, &job).map_err(io::Error::other)?;
    writeln!(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_job_commands_before_contacting_server() -> io::Result<()> {
        let id = "a".repeat(32);
        assert!(parse(["run", &id, "step", "1"].into_iter().map(OsString::from)).is_ok());
        assert!(parse(["bind", &id, &id, "2", "1"].into_iter().map(OsString::from)).is_ok());
        for args in [
            vec!["run", &id, "step", "0"],
            vec!["status", "../bad"],
            vec!["cancel", &id, "--force"],
            vec!["bind", &id, &id, "2"],
        ] {
            assert!(parse(args.into_iter().map(OsString::from)).is_err());
        }
        Ok(())
    }
}
