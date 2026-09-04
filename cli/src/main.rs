mod app;
mod runner;
mod session;

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

const USAGE: &str = concat!(
    "usage: jio <command> [arguments]\n",
    "\n",
    "  Command   Arguments\n",
    "  run       <source> [options]                     Run source code in Jio\n",
    "  create    [options]                              Create a persistent VM\n",
    "  exec      <session-id> <command> [options]       Run a command in a VM\n",
    "  connect   [session-id] [options]                 Open an interactive shell\n",
    "  stop      <session-id> [options]                 Stop compute and retain files\n",
    "  start     <session-id> [options]                 Start with retained files\n",
    "  destroy   [session-id] [options]                 Delete the VM and retained files",
);

const RUN_USAGE: &str = concat!(
    "usage: jio run <source> [options]\n",
    "\n",
    "  <source>                   Rust or Python source file\n",
    "  --language python          Treat <source> as inline Python\n",
    "  --instances <count>        Number of VMs to run (default: 1)\n",
    "  --concurrency <count>      Maximum VMs running at once\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const CREATE_USAGE: &str = concat!(
    "usage: jio create [options]\n",
    "\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const EXEC_USAGE: &str = concat!(
    "usage: jio exec <session-id> <command> [options]\n",
    "\n",
    "  <session-id>               VM session ID\n",
    "  <command>                  Quoted shell command to run\n",
    "  --timeout <seconds>        Command timeout\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const CONNECT_USAGE: &str = concat!(
    "usage: jio connect [session-id] [options]\n",
    "\n",
    "  [session-id]               VM session ID; defaults to the current session\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const STOP_USAGE: &str = concat!(
    "usage: jio stop <session-id> [options]\n",
    "\n",
    "  <session-id>               VM session ID\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const START_USAGE: &str = concat!(
    "usage: jio start <session-id> [options]\n",
    "\n",
    "  <session-id>               VM session ID\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const DESTROY_USAGE: &str = concat!(
    "usage: jio destroy [session-id] [options]\n",
    "\n",
    "  [session-id]               VM session ID; defaults to the current session\n",
    "  --yes                      Skip the destructive-action confirmation\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

enum Invocation {
    Help(&'static str),
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
        id: Option<String>,
    },
    Exec {
        host: String,
        id: String,
        command: String,
        timeout: Duration,
    },
    Destroy {
        host: String,
        id: Option<String>,
        yes: bool,
    },
    Stop {
        host: String,
        id: String,
    },
    Start {
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
        Invocation::Help(help) => {
            println!("{help}");
            Ok(())
        }
        Invocation::Run {
            source,
            host,
            instances,
            concurrency,
        } => app::run(source, host, instances, concurrency),
        Invocation::Create { host } => create(host),
        Invocation::Connect { host, id } => session::connect(host, id.as_deref()),
        Invocation::Exec {
            host,
            id,
            command,
            timeout,
        } => {
            let result = session::exec(host, &id, &command, timeout)?;
            io::stdout().write_all(&result.stdout)?;
            io::stderr().write_all(&result.stderr)?;
            if result.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!(
                    "guest command exited with status {}",
                    result.exit_code
                )))
            }
        }
        Invocation::Destroy { host, id, yes } => destroy(host, id.as_deref(), yes),
        Invocation::Stop { host, id } => {
            let stopped = session::stop(host, &id)?;
            println!(
                "{}\tstopped\tgeneration {}",
                stopped.session_id, stopped.generation
            );
            Ok(())
        }
        Invocation::Start { host, id } => {
            let started = session::start(host, &id)?;
            println!(
                "{}\tready\tgeneration {}",
                started.session_id, started.generation
            );
            Ok(())
        }
    }
}

fn create(host: String) -> io::Result<()> {
    println!("{}", session::create(host)?.session_id);
    Ok(())
}

fn destroy(host: String, id: Option<&str>, yes: bool) -> io::Result<()> {
    let id = session::target_session_id(&host, id)?;
    if !yes {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "jio destroy requires an interactive terminal or --yes",
            ));
        }
        let input = io::stdin();
        let output = io::stdout();
        let mut input = input.lock();
        let mut output = output.lock();
        if !prompt_destroy(&mut input, &mut output, &id)? {
            writeln!(output, "Cancelled.")?;
            return Ok(());
        }
    }
    session::destroy(host, &id)
}

fn options() -> io::Result<Invocation> {
    options_from(env::args_os().skip(1))
}

fn options_from(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    match arguments.next().as_deref() {
        Some(command) if is_help(command) => Ok(Invocation::Help(USAGE)),
        Some(command) if command == OsStr::new("run") => run_options(arguments),
        Some(command) if command == OsStr::new("create") => create_options(arguments),
        Some(command) if command == OsStr::new("connect") => connect_options(arguments),
        Some(command) if command == OsStr::new("exec") => exec_options(arguments),
        Some(command) if command == OsStr::new("stop") => {
            session_options(arguments, SessionAction::Stop)
        }
        Some(command) if command == OsStr::new("start") => {
            session_options(arguments, SessionAction::Start)
        }
        Some(command) if command == OsStr::new("destroy") => destroy_options(arguments),
        _ => Err(usage(USAGE)),
    }
}

fn connect_options(arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    match optional_session_target(arguments, CONNECT_USAGE)? {
        Some((host, id)) => Ok(Invocation::Connect { host, id }),
        None => Ok(Invocation::Help(CONNECT_USAGE)),
    }
}

fn optional_session_target(
    arguments: impl Iterator<Item = OsString>,
    command_usage: &'static str,
) -> io::Result<Option<(String, Option<String>)>> {
    let mut id = None;
    let mut host = None;
    let mut arguments = arguments;
    while let Some(argument) = arguments.next() {
        if is_help(&argument) {
            return Ok(None);
        }
        if argument == "--host" && host.is_none() {
            host = Some(arguments.next().ok_or_else(|| usage(command_usage))?);
        } else if id.is_none() {
            let value = text(Some(argument), "session ID", command_usage)?;
            if !jio_client::valid_session_id(&value) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session ID is invalid",
                ));
            }
            id = Some(value);
        } else {
            return Err(usage(command_usage));
        }
    }
    Ok(Some((session::host(host)?, id)))
}

fn run_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let source = arguments.next().ok_or_else(|| usage(RUN_USAGE))?;
    if is_help(&source) {
        return Ok(Invocation::Help(RUN_USAGE));
    }
    let mut host = env::var("JIO_ENDPOINT")
        .ok()
        .or_else(|| env::var("JIO_HOST").ok());
    let mut instances = 1;
    let mut concurrency = None;
    let mut language = None;

    while let Some(argument) = arguments.next() {
        if argument == "--host" {
            host = Some(text(arguments.next(), "host", RUN_USAGE)?);
        } else if argument == "--instances" {
            instances = text(arguments.next(), "instance count", RUN_USAGE)?
                .parse()
                .map_err(|_| usage(RUN_USAGE))?;
        } else if argument == "--concurrency" {
            concurrency = Some(
                text(arguments.next(), "concurrency", RUN_USAGE)?
                    .parse()
                    .map_err(|_| usage(RUN_USAGE))?,
            );
        } else if argument == "--language" {
            language = Some(text(arguments.next(), "language", RUN_USAGE)?);
        } else if is_help(&argument) {
            return Ok(Invocation::Help(RUN_USAGE));
        } else {
            return Err(usage(RUN_USAGE));
        }
    }

    let host = host.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "missing --host <user@address|https://url> (or JIO_ENDPOINT/JIO_HOST)",
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

fn exec_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let first = arguments.next();
    if first.as_deref().is_some_and(is_help) {
        return Ok(Invocation::Help(EXEC_USAGE));
    }
    let id = text(first, "session ID", EXEC_USAGE)?;
    if !jio_client::valid_session_id(&id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session ID is invalid",
        ));
    }
    let command = text(arguments.next(), "command", EXEC_USAGE)?;
    if command.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "command must not be empty",
        ));
    }

    let mut host = None;
    let mut timeout = jio_client::DEFAULT_COMMAND_TIMEOUT;
    let mut timeout_set = false;
    while let Some(argument) = arguments.next() {
        if argument == "--host" && host.is_none() {
            host = Some(arguments.next().ok_or_else(|| usage(EXEC_USAGE))?);
        } else if argument == "--timeout" && !timeout_set {
            let seconds = text(arguments.next(), "timeout", EXEC_USAGE)?
                .parse::<u64>()
                .map_err(|_| usage(EXEC_USAGE))?;
            timeout = Duration::from_secs(seconds);
            timeout_set = true;
        } else if is_help(&argument) {
            return Ok(Invocation::Help(EXEC_USAGE));
        } else {
            return Err(usage(EXEC_USAGE));
        }
    }
    if timeout.is_zero() || timeout > jio_client::MAX_COMMAND_TIMEOUT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "timeout must be between 1 and {} seconds",
                jio_client::MAX_COMMAND_TIMEOUT.as_secs()
            ),
        ));
    }

    Ok(Invocation::Exec {
        host: session::host(host)?,
        id,
        command,
        timeout,
    })
}

fn create_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let mut host = None;
    while let Some(argument) = arguments.next() {
        if argument == "--host" {
            if host.is_some() {
                return Err(usage(CREATE_USAGE));
            }
            host = Some(arguments.next().ok_or_else(|| usage(CREATE_USAGE))?);
        } else if is_help(&argument) {
            return Ok(Invocation::Help(CREATE_USAGE));
        } else {
            return Err(usage(CREATE_USAGE));
        }
    }
    Ok(Invocation::Create {
        host: session::host(host)?,
    })
}

fn destroy_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let mut id = None;
    let mut host = None;
    let mut yes = false;
    while let Some(argument) = arguments.next() {
        if is_help(&argument) {
            return Ok(Invocation::Help(DESTROY_USAGE));
        }
        if argument == "--host" && host.is_none() {
            host = Some(arguments.next().ok_or_else(|| usage(DESTROY_USAGE))?);
        } else if argument == "--yes" && !yes {
            yes = true;
        } else if id.is_none() {
            let value = text(Some(argument), "session ID", DESTROY_USAGE)?;
            if !jio_client::valid_session_id(&value) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session ID is invalid",
                ));
            }
            id = Some(value);
        } else {
            return Err(usage(DESTROY_USAGE));
        }
    }
    Ok(Invocation::Destroy {
        host: session::host(host)?,
        id,
        yes,
    })
}

#[derive(Clone, Copy)]
enum SessionAction {
    Stop,
    Start,
}

fn session_options(
    mut arguments: impl Iterator<Item = OsString>,
    action: SessionAction,
) -> io::Result<Invocation> {
    let command_usage = action.usage();
    let first = arguments.next();
    if first.as_deref().is_some_and(is_help) {
        return Ok(Invocation::Help(command_usage));
    }
    let id = text(first, "session ID", command_usage)?;
    if !jio_client::valid_session_id(&id) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "session ID is invalid",
        ));
    }
    let mut host = None;
    while let Some(argument) = arguments.next() {
        if is_help(&argument) {
            return Ok(Invocation::Help(command_usage));
        }
        if argument != "--host" || host.is_some() {
            return Err(usage(command_usage));
        }
        host = Some(arguments.next().ok_or_else(|| usage(command_usage))?);
    }
    let host = session::host(host)?;
    match action {
        SessionAction::Stop => Ok(Invocation::Stop { host, id }),
        SessionAction::Start => Ok(Invocation::Start { host, id }),
    }
}

impl SessionAction {
    fn usage(self) -> &'static str {
        match self {
            Self::Stop => STOP_USAGE,
            Self::Start => START_USAGE,
        }
    }
}

fn text(value: Option<OsString>, name: &str, command_usage: &'static str) -> io::Result<String> {
    value
        .ok_or_else(|| usage(command_usage))?
        .into_string()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is not UTF-8")))
}

fn is_help(value: &OsStr) -> bool {
    value == OsStr::new("--help") || value == OsStr::new("-h")
}

fn prompt_destroy(input: &mut impl BufRead, output: &mut impl Write, id: &str) -> io::Result<bool> {
    writeln!(output, "Destroy VM {id} and all retained files?")?;
    writeln!(output, "\x1b[31mThis action cannot be undone.\x1b[0m")?;
    prompt_yes_no(input, output, "Continue? [y/N] ", false)
}

fn prompt_yes_no(
    input: &mut impl BufRead,
    output: &mut impl Write,
    prompt: &str,
    default: bool,
) -> io::Result<bool> {
    const MAX_ANSWER_BYTES: u64 = 32;
    loop {
        write!(output, "{prompt}")?;
        output.flush()?;

        let mut answer = String::new();
        let bytes = input.take(MAX_ANSWER_BYTES).read_line(&mut answer)?;
        if bytes == 0 {
            return Ok(false);
        }
        if bytes == MAX_ANSWER_BYTES as usize && !answer.ends_with('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "answer is too long",
            ));
        }

        let answer = answer.trim();
        if answer.is_empty() {
            return Ok(default);
        }
        if answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes") {
            return Ok(true);
        }
        if answer.eq_ignore_ascii_case("n") || answer.eq_ignore_ascii_case("no") {
            return Ok(false);
        }
        writeln!(output, "Please answer Yes or No.")?;
    }
}

fn usage(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

#[cfg(test)]
mod tests {
    use super::{
        CONNECT_USAGE, CREATE_USAGE, DESTROY_USAGE, EXEC_USAGE, Invocation, RUN_USAGE, START_USAGE,
        STOP_USAGE, USAGE, options_from, prompt_destroy,
    };
    use std::ffi::OsString;
    use std::io::Cursor;
    use std::time::Duration;

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
    fn connects_to_an_existing_session() {
        let invocation = options_from(arguments("connect"));
        assert!(matches!(
            invocation,
            Ok(Invocation::Connect { id: Some(id), .. })
                if id == "abababababababababababababababab"
        ));
    }

    #[test]
    fn connects_to_the_current_session_without_an_id() {
        let invocation = options_from(
            ["connect", "--host", "ubuntu@host"]
                .into_iter()
                .map(OsString::from),
        );
        assert!(matches!(
            invocation,
            Ok(Invocation::Connect { id: None, .. })
        ));
    }

    #[test]
    fn parses_stop_start_and_destroy() {
        assert!(matches!(
            options_from(arguments("stop")),
            Ok(Invocation::Stop { .. })
        ));
        assert!(matches!(
            options_from(arguments("start")),
            Ok(Invocation::Start { .. })
        ));
        assert!(matches!(
            options_from(arguments("destroy")),
            Ok(Invocation::Destroy {
                id: Some(_),
                yes: false,
                ..
            })
        ));
    }

    #[test]
    fn destroys_the_current_session_with_explicit_confirmation_bypass() {
        let invocation = options_from(
            ["destroy", "--yes", "--host", "ubuntu@host"]
                .into_iter()
                .map(OsString::from),
        );
        assert!(matches!(
            invocation,
            Ok(Invocation::Destroy {
                id: None,
                yes: true,
                ..
            })
        ));
    }

    #[test]
    fn parses_a_bounded_session_command() {
        let invocation = options_from(
            [
                "exec",
                "abababababababababababababababab",
                "sudo apt-get update -qq",
                "--timeout",
                "600",
                "--host",
                "ubuntu@host",
            ]
            .into_iter()
            .map(OsString::from),
        );
        assert!(matches!(
            invocation,
            Ok(Invocation::Exec {
                id,
                command,
                timeout,
                ..
            }) if id == "abababababababababababababababab"
                && command == "sudo apt-get update -qq"
                && timeout == Duration::from_secs(600)
        ));
    }

    #[test]
    fn requires_yes_to_destroy() {
        for (answer, expected) in [("\n", false), ("no\n", false), ("yes\n", true)] {
            let mut input = Cursor::new(answer.as_bytes());
            let mut output = Vec::new();
            assert_eq!(
                prompt_destroy(&mut input, &mut output, "abababababababababababababababab").ok(),
                Some(expected)
            );
            assert_eq!(
                String::from_utf8(output).ok().as_deref(),
                Some(concat!(
                    "Destroy VM abababababababababababababababab and all retained files?\n",
                    "\x1b[31mThis action cannot be undone.\x1b[0m\n",
                    "Continue? [y/N] "
                ))
            );
        }
    }

    #[test]
    fn create_has_no_connect_flag() {
        assert!(options_from(["create", "--connect"].into_iter().map(OsString::from)).is_err());
    }

    #[test]
    fn bare_invocation_lists_commands_in_readable_columns() {
        let message = options_from(std::iter::empty())
            .err()
            .map(|error| error.to_string());
        assert_eq!(message.as_deref(), Some(USAGE));
    }

    #[test]
    fn incomplete_commands_show_their_own_usage() {
        for (command, expected) in [
            ("run", RUN_USAGE),
            ("exec", EXEC_USAGE),
            ("stop", STOP_USAGE),
            ("start", START_USAGE),
        ] {
            let message = options_from([command].into_iter().map(OsString::from))
                .err()
                .map(|error| error.to_string());
            assert_eq!(message.as_deref(), Some(expected));
        }
    }

    #[test]
    fn every_command_supports_help() {
        for (command, expected) in [
            ("run", RUN_USAGE),
            ("create", CREATE_USAGE),
            ("exec", EXEC_USAGE),
            ("connect", CONNECT_USAGE),
            ("stop", STOP_USAGE),
            ("start", START_USAGE),
            ("destroy", DESTROY_USAGE),
        ] {
            let invocation = options_from([command, "--help"].into_iter().map(OsString::from));
            assert!(matches!(invocation, Ok(Invocation::Help(help)) if help == expected));
        }
    }
}
