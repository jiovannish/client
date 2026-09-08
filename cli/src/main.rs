mod app;
mod config;
mod runner;
mod session;

use std::env;
use std::ffi::{OsStr, OsString};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const USAGE: &str = concat!(
    "usage: jio <command> [arguments]\n",
    "\n",
    "  Command   Arguments\n",
    "  run       <source> [options]                     Run source code in Jio\n",
    "  create    [options]                              Create a persistent VM\n",
    "  config                                           Configure Jio defaults\n",
    "  usage     [options]                              Show account limits and reservations\n",
    "  list      [options]                              List VM IDs and sizes\n",
    "  exec      <session-id> <command> [options]       Run a command in a VM\n",
    "  connect   [session-id] [options]                 Open an interactive shell\n",
    "  stop      <session-id> [options]                 Stop compute and retain files\n",
    "  start     <session-id> [options]                 Start with retained files\n",
    "  destroy   [session-id] [options]                 Delete the VM and retained files\n",
    "  login     <api-key> [options]                    Save your Jio API key\n",
    "  login     codex [session-id] [options]           Use local Codex login in a VM\n",
    "  yolo      <agent> [session-id] [options]         Open an agent with full VM access\n",
    "  completion <shell>                               Print shell completion setup\n",
    "  --version                                        Print the installed version",
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

const ACCOUNT_USAGE: &str = concat!(
    "usage: jio usage [options]\n\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST\n\n",
    "Show the current API key's shared account limits and allocations from the control plane.",
);

const CONFIG_USAGE: &str = concat!(
    "usage: jio config\n",
    "\n",
    "Interactively choose the default VM size used by jio create.",
);

const LIST_USAGE: &str = concat!(
    "usage: jio list [options]\n\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST\n\n",
    "List non-destroyed VMs and sizes for the current API key's account.",
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

const LOGIN_USAGE: &str = concat!(
    "usage: jio login <api-key> [--host <host>]\n",
    "       jio login codex [session-id] [options]\n",
    "\n",
    "Verify and save your Jio API key for later CLI commands.\n",
    "JIO_API_KEY overrides the saved login.\n",
    "\n",
    "  Agent     Arguments\n",
    "  codex     [session-id] [options]                 Use local Codex login in a VM\n",
    "\n",
    "  [session-id]               VM session ID; defaults to the current session\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST",
);

const YOLO_USAGE: &str = concat!(
    "usage: jio yolo <agent> [session-id] [options]\n",
    "\n",
    "  Agent     Arguments\n",
    "  codex     [session-id] [options]                 Open Codex without approvals or sandbox\n",
    "\n",
    "  [session-id]               VM session ID; defaults to the current session\n",
    "  --host <host>              Override JIO_ENDPOINT or JIO_HOST\n",
    "\n",
    "Transfers the local Codex login, then opens Codex with approvals and its sandbox disabled.",
);

const COMPLETION_USAGE: &str = concat!(
    "usage: jio completion <shell>\n",
    "\n",
    "  Shell\n",
    "  zsh                       Print completion setup for Zsh\n",
    "  bash                      Print completion setup for Bash\n",
    "  fish                      Print completion setup for Fish",
);

const ZSH_COMPLETION: &str = r#"#compdef jio

if (( ! $+functions[compdef] )); then
  autoload -Uz compinit
  compinit
fi

_jio() {
  local -a commands agents shells options
  commands=(run create config usage list exec connect stop start destroy login yolo completion)
  agents=(codex)
  shells=(zsh bash fish)

  if (( CURRENT == 2 )); then
    compadd -- $commands
    return
  fi

  case "${words[2]}" in
    login|yolo)
      if (( CURRENT == 3 )); then
        compadd -- $agents
        return
      fi
      options=(--host --help -h)
      ;;
    completion)
      if (( CURRENT == 3 )); then
        compadd -- $shells
        return
      fi
      options=(--help -h)
      ;;
    config)
      options=(--help -h)
      ;;
    run)
      options=(--language --instances --concurrency --host --help -h)
      ;;
    exec)
      options=(--timeout --host --help -h)
      ;;
    destroy)
      options=(--yes --host --help -h)
      ;;
    usage|list|create|connect|stop|start)
      options=(--host --help -h)
      ;;
  esac
  if [[ "$PREFIX" == -* ]]; then
    compadd -- $options
  fi
}

compdef _jio jio"#;

const BASH_COMPLETION: &str = r#"_jio_completion() {
  local current command
  current="${COMP_WORDS[COMP_CWORD]}"
  command="${COMP_WORDS[1]}"

  if [[ $COMP_CWORD -eq 1 ]]; then
    COMPREPLY=($(compgen -W 'run create config usage list exec connect stop start destroy login yolo completion' -- "$current"))
    return
  fi
  if [[ $COMP_CWORD -eq 2 && ( $command == login || $command == yolo ) ]]; then
    COMPREPLY=($(compgen -W 'codex' -- "$current"))
    return
  fi
  if [[ $COMP_CWORD -eq 2 && $command == completion ]]; then
    COMPREPLY=($(compgen -W 'zsh bash fish' -- "$current"))
    return
  fi

  case "$command" in
    config)    COMPREPLY=($(compgen -W '--help -h' -- "$current")) ;;
    run)       COMPREPLY=($(compgen -W '--language --instances --concurrency --host --help -h' -- "$current")) ;;
    exec)      COMPREPLY=($(compgen -W '--timeout --host --help -h' -- "$current")) ;;
    destroy)   COMPREPLY=($(compgen -W '--yes --host --help -h' -- "$current")) ;;
    usage|list|login|yolo|create|connect|stop|start)
               COMPREPLY=($(compgen -W '--host --help -h' -- "$current")) ;;
  esac
}

complete -F _jio_completion jio"#;

const FISH_COMPLETION: &str = r#"complete -c jio -f
complete -c jio -n '__fish_use_subcommand' -a 'run create config usage list exec connect stop start destroy login yolo completion'
complete -c jio -n '__fish_seen_subcommand_from login yolo; and test (count (commandline -opc)) -eq 2' -a codex
complete -c jio -n '__fish_seen_subcommand_from completion; and test (count (commandline -opc)) -eq 2' -a 'zsh bash fish'
complete -c jio -n '__fish_seen_subcommand_from run' -l language -l instances -l concurrency -l host
complete -c jio -n '__fish_seen_subcommand_from exec' -l timeout -l host
complete -c jio -n '__fish_seen_subcommand_from destroy' -l yes -l host
complete -c jio -n '__fish_seen_subcommand_from usage list create connect stop start login yolo' -l host"#;

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
    Config,
    Usage {
        host: String,
    },
    List {
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
    Login {
        host: String,
        api_key: String,
    },
    LoginCodex {
        host: String,
        id: Option<String>,
    },
    YoloCodex {
        host: String,
        id: Option<String>,
    },
    Completion {
        shell: CompletionShell,
    },
}

#[derive(Clone, Copy)]
enum CompletionShell {
    Zsh,
    Bash,
    Fish,
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
        Invocation::Config => config::run(),
        Invocation::Usage { host } => session::usage(host),
        Invocation::List { host } => session::list(host),
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
        Invocation::Login { host, api_key } => config::login(&host, &api_key),
        Invocation::LoginCodex { host, id } => {
            let (id, status) = session::codex_login(host, id.as_deref())?;
            io::stdout().write_all(&status.stdout)?;
            io::stderr().write_all(&status.stderr)?;
            println!("Codex login installed in VM {id}.");
            Ok(())
        }
        Invocation::YoloCodex { host, id } => session::codex_yolo(host, id.as_deref()),
        Invocation::Completion { shell } => {
            println!("{}", shell.script());
            Ok(())
        }
    }
}

fn create(host: String) -> io::Result<()> {
    let size = config::load()?.size;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        println!("{}", session::create(host, size)?.session_id);
        return Ok(());
    }

    let (report_id, receive_id) = mpsc::sync_channel(1);
    let (report_acceptance, receive_acceptance) = mpsc::sync_channel(1);
    let (report_ready, receive_ready) = mpsc::sync_channel(1);
    let create_host = host.clone();
    thread::spawn(move || {
        let mut id_reported = false;
        let accepted = session::accept_create_and_report_id(create_host.clone(), size, |id| {
            id_reported = true;
            let _ = report_id.send(Ok(id.to_owned()));
        });
        match accepted {
            Ok(id) => {
                if report_acceptance.send(Ok(id.clone())).is_ok() {
                    let ready = session::prepare_connect(create_host, &id);
                    let _ = report_ready.send(ready);
                }
            }
            Err(failure) => {
                if id_reported {
                    let _ = report_acceptance.send(Err(failure));
                } else {
                    let _ = report_id.send(Err(failure));
                }
            }
        }
    });

    let id = receive_id
        .recv()
        .map_err(|_| io::Error::other("session creation ended before reporting its ID"))??;
    let connect = (|| {
        let input = io::stdin();
        let output = io::stdout();
        let mut input = input.lock();
        let mut output = output.lock();
        writeln!(output, "Creating VM: {id}")?;
        prompt_connect(&mut input, &mut output)
    })()?;
    let accepted = receive_acceptance
        .recv()
        .map_err(|_| io::Error::other("session creation ended before Core accepted it"))??;
    if accepted != id {
        return Err(io::Error::other("accepted session ID changed"));
    }
    if !connect {
        return Ok(());
    }
    receive_ready
        .recv()
        .map_err(|_| io::Error::other("session readiness watcher ended without a result"))??
        .connect()
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
        Some(command) if command == OsStr::new("--version") || command == OsStr::new("-V") => {
            if arguments.next().is_some() {
                return Err(usage(USAGE));
            }
            Ok(Invocation::Help(concat!("jio ", env!("CARGO_PKG_VERSION"))))
        }
        Some(command) if is_help(command) => Ok(Invocation::Help(USAGE)),
        Some(command) if command == OsStr::new("run") => run_options(arguments),
        Some(command) if command == OsStr::new("create") => {
            host_options(arguments, CREATE_USAGE, |host| Invocation::Create { host })
        }
        Some(command) if command == OsStr::new("usage") => {
            host_options(arguments, ACCOUNT_USAGE, |host| Invocation::Usage { host })
        }
        Some(command) if command == OsStr::new("list") => {
            host_options(arguments, LIST_USAGE, |host| Invocation::List { host })
        }
        Some(command) if command == OsStr::new("config") => config_options(arguments),
        Some(command) if command == OsStr::new("connect") => connect_options(arguments),
        Some(command) if command == OsStr::new("exec") => exec_options(arguments),
        Some(command) if command == OsStr::new("stop") => {
            session_options(arguments, SessionAction::Stop)
        }
        Some(command) if command == OsStr::new("start") => {
            session_options(arguments, SessionAction::Start)
        }
        Some(command) if command == OsStr::new("destroy") => destroy_options(arguments),
        Some(command) if command == OsStr::new("login") => login_options(arguments),
        Some(command) if command == OsStr::new("yolo") => {
            agent_options(arguments, AgentAction::Yolo)
        }
        Some(command) if command == OsStr::new("completion") => completion_options(arguments),
        _ => Err(usage(USAGE)),
    }
}

fn login_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let first = arguments.next();
    if first.as_deref().is_some_and(is_help) {
        return Ok(Invocation::Help(LOGIN_USAGE));
    }
    if first.as_deref() == Some(OsStr::new("codex")) {
        return agent_session_options(arguments, AgentAction::Login);
    }
    let api_key = text(first, "API key", LOGIN_USAGE)?;
    host_options(arguments, LOGIN_USAGE, |host| Invocation::Login {
        host,
        api_key,
    })
}

fn config_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    match arguments.next() {
        None => Ok(Invocation::Config),
        Some(argument) if is_help(&argument) && arguments.next().is_none() => {
            Ok(Invocation::Help(CONFIG_USAGE))
        }
        Some(_) => Err(usage(CONFIG_USAGE)),
    }
}

fn completion_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let shell = match arguments.next().as_deref() {
        Some(shell) if is_help(shell) => return Ok(Invocation::Help(COMPLETION_USAGE)),
        Some(shell) if shell == OsStr::new("zsh") => CompletionShell::Zsh,
        Some(shell) if shell == OsStr::new("bash") => CompletionShell::Bash,
        Some(shell) if shell == OsStr::new("fish") => CompletionShell::Fish,
        _ => return Err(usage(COMPLETION_USAGE)),
    };
    if arguments.next().is_some() {
        return Err(usage(COMPLETION_USAGE));
    }
    Ok(Invocation::Completion { shell })
}

impl CompletionShell {
    fn script(self) -> &'static str {
        match self {
            Self::Zsh => ZSH_COMPLETION,
            Self::Bash => BASH_COMPLETION,
            Self::Fish => FISH_COMPLETION,
        }
    }
}

fn connect_options(arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    match optional_session_target(arguments, CONNECT_USAGE)? {
        Some((host, id)) => Ok(Invocation::Connect { host, id }),
        None => Ok(Invocation::Help(CONNECT_USAGE)),
    }
}

#[derive(Clone, Copy)]
enum AgentAction {
    Login,
    Yolo,
}

fn agent_options(
    mut arguments: impl Iterator<Item = OsString>,
    action: AgentAction,
) -> io::Result<Invocation> {
    let command_usage = action.usage();
    match arguments.next().as_deref() {
        Some(agent) if is_help(agent) => Ok(Invocation::Help(command_usage)),
        Some(agent) if agent == OsStr::new("codex") => agent_session_options(arguments, action),
        _ => Err(usage(command_usage)),
    }
}

fn agent_session_options(
    arguments: impl Iterator<Item = OsString>,
    action: AgentAction,
) -> io::Result<Invocation> {
    let command_usage = action.usage();
    let Some((host, id)) = optional_session_target(arguments, command_usage)? else {
        return Ok(Invocation::Help(command_usage));
    };
    match action {
        AgentAction::Login => Ok(Invocation::LoginCodex { host, id }),
        AgentAction::Yolo => Ok(Invocation::YoloCodex { host, id }),
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

impl AgentAction {
    fn usage(self) -> &'static str {
        match self {
            Self::Login => LOGIN_USAGE,
            Self::Yolo => YOLO_USAGE,
        }
    }
}

fn run_options(mut arguments: impl Iterator<Item = OsString>) -> io::Result<Invocation> {
    let source = arguments.next().ok_or_else(|| usage(RUN_USAGE))?;
    if is_help(&source) {
        return Ok(Invocation::Help(RUN_USAGE));
    }
    let mut host = None;
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

    let host = session::host(host.map(OsString::from))?;
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

fn host_options(
    mut arguments: impl Iterator<Item = OsString>,
    help: &'static str,
    invocation: impl FnOnce(String) -> Invocation,
) -> io::Result<Invocation> {
    let mut host = None;
    while let Some(argument) = arguments.next() {
        if argument == "--host" {
            if host.is_some() {
                return Err(usage(help));
            }
            host = Some(arguments.next().ok_or_else(|| usage(help))?);
        } else if is_help(&argument) {
            return Ok(Invocation::Help(help));
        } else {
            return Err(usage(help));
        }
    }
    let host = session::host(host)?;
    Ok(invocation(host))
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

fn prompt_connect(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<bool> {
    prompt_yes_no(input, output, "Do you want to connect now? [Y/n] ", true)
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
        ACCOUNT_USAGE, COMPLETION_USAGE, CONFIG_USAGE, CONNECT_USAGE, CREATE_USAGE,
        CompletionShell, DESTROY_USAGE, EXEC_USAGE, Invocation, LIST_USAGE, LOGIN_USAGE, RUN_USAGE,
        START_USAGE, STOP_USAGE, USAGE, YOLO_USAGE, options_from, prompt_connect, prompt_destroy,
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
    fn usage_accepts_only_host_options() {
        assert!(
            matches!(options_from(["usage", "--host", "https://api.jio.dev"].into_iter().map(OsString::from)),
            Ok(Invocation::Usage { host }) if host == "https://api.jio.dev")
        );
        for args in [
            vec!["usage", "alice"],
            vec!["usage", "--host"],
            vec!["usage", "--host", "a", "--host", "b"],
        ] {
            assert!(options_from(args.into_iter().map(OsString::from)).is_err());
        }
    }

    #[test]
    fn list_accepts_only_host_options() {
        assert!(
            matches!(options_from(["list", "--host", "https://api.jio.dev"].into_iter().map(OsString::from)),
            Ok(Invocation::List { host }) if host == "https://api.jio.dev")
        );
        for args in [
            vec!["list", "extra"],
            vec!["list", "--host"],
            vec!["list", "--host", "a", "--host", "b"],
        ] {
            assert!(options_from(args.into_iter().map(OsString::from)).is_err());
        }
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
    fn parses_login_codex_for_an_existing_session() {
        let invocation = options_from(
            [
                "login",
                "codex",
                "abababababababababababababababab",
                "--host",
                "ubuntu@host",
            ]
            .into_iter()
            .map(OsString::from),
        );
        assert!(matches!(
            invocation,
            Ok(Invocation::LoginCodex { id: Some(id), .. })
                if id == "abababababababababababababababab"
        ));
    }

    #[test]
    fn parses_yolo_codex_with_the_current_session() {
        let invocation = options_from(
            ["yolo", "codex", "--host", "ubuntu@host"]
                .into_iter()
                .map(OsString::from),
        );
        assert!(matches!(
            invocation,
            Ok(Invocation::YoloCodex { id: None, .. })
        ));
    }

    #[test]
    fn rejects_the_old_nested_codex_command() {
        assert!(options_from(["codex", "yolo"].into_iter().map(OsString::from)).is_err());
    }

    #[test]
    fn prints_completion_with_nested_codex_agents() {
        let invocation = options_from(["completion", "zsh"].into_iter().map(OsString::from));
        assert!(matches!(
            invocation,
            Ok(Invocation::Completion {
                shell: CompletionShell::Zsh
            })
        ));
        assert!(CompletionShell::Zsh.script().contains("agents=(codex)"));
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
    fn asks_whether_to_connect_after_creation() {
        for (answer, expected) in [("\n", true), ("Yes\n", true), ("n\n", false)] {
            let mut input = Cursor::new(answer.as_bytes());
            let mut output = Vec::new();
            assert_eq!(prompt_connect(&mut input, &mut output).ok(), Some(expected));
            assert_eq!(
                String::from_utf8(output).ok().as_deref(),
                Some("Do you want to connect now? [Y/n] ")
            );
        }
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
    fn parses_config_without_requiring_an_endpoint() {
        assert!(matches!(
            options_from(["config"].into_iter().map(OsString::from)),
            Ok(Invocation::Config)
        ));
        assert!(options_from(["config", "extra"].into_iter().map(OsString::from)).is_err());
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
            ("login", LOGIN_USAGE),
            ("yolo", YOLO_USAGE),
            ("completion", COMPLETION_USAGE),
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
            ("usage", ACCOUNT_USAGE),
            ("list", LIST_USAGE),
            ("config", CONFIG_USAGE),
            ("exec", EXEC_USAGE),
            ("connect", CONNECT_USAGE),
            ("stop", STOP_USAGE),
            ("start", START_USAGE),
            ("destroy", DESTROY_USAGE),
            ("login", LOGIN_USAGE),
            ("yolo", YOLO_USAGE),
            ("completion", COMPLETION_USAGE),
        ] {
            let invocation = options_from([command, "--help"].into_iter().map(OsString::from));
            assert!(matches!(invocation, Ok(Invocation::Help(help)) if help == expected));
        }

        let invocation = options_from(["login", "codex", "--help"].into_iter().map(OsString::from));
        assert!(matches!(invocation, Ok(Invocation::Help(help)) if help == LOGIN_USAGE));
        let invocation = options_from(["yolo", "codex", "--help"].into_iter().map(OsString::from));
        assert!(matches!(invocation, Ok(Invocation::Help(help)) if help == YOLO_USAGE));
    }
}
