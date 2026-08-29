use jio_client::{Event as ClientEvent, RunRequest};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub enum Event {
    Phase(String),
    Compiled(Duration),
    Client(ClientEvent),
    Done(Duration),
    Error(String),
}

pub enum Source {
    File(PathBuf),
    Python(String),
}

impl Source {
    pub fn is_interpreted(&self) -> bool {
        match self {
            Self::File(path) => path.extension().is_some_and(|extension| extension == "py"),
            Self::Python(_) => true,
        }
    }
}

pub fn start(source: Source, host: String, instances: usize) -> Receiver<Event> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        if let Err(error) = run(&source, &host, instances, &sender) {
            let _ = sender.send(Event::Error(error.to_string()));
        }
    });
    receiver
}

fn run(source: &Source, host: &str, instances: usize, sender: &Sender<Event>) -> io::Result<()> {
    let started = Instant::now();
    let temporary = temporary_directory()?;
    let result = execute(source, host, instances, sender, &temporary);
    let cleanup = fs::remove_dir_all(&temporary);
    result?;
    cleanup?;
    send(sender, Event::Done(started.elapsed()))
}

fn execute(
    source: &Source,
    host: &str,
    instances: usize,
    sender: &Sender<Event>,
    temporary: &Path,
) -> io::Result<()> {
    let program = temporary.join("program");
    let inline = temporary.join("source.py");
    let api_key = env::var("JIO_API_KEY")
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "JIO_API_KEY is required"))?;
    let (workload, runtime) = match source {
        Source::File(source) if source.extension().is_some_and(|value| value == "rs") => {
            send(
                sender,
                Event::Phase("Cross-compiling Rust for Linux".into()),
            )?;
            let compile_started = Instant::now();
            let compile = Command::new("rustup")
                .arg("run")
                .arg("1.85.1")
                .arg("rustc")
                .arg("--edition=2024")
                .arg("--target=x86_64-unknown-linux-musl")
                .arg("-C")
                .arg("linker=rust-lld")
                .arg("-C")
                .arg("opt-level=3")
                .arg("-C")
                .arg("panic=abort")
                .arg("-C")
                .arg("strip=symbols")
                .arg(source)
                .arg("-o")
                .arg(&program)
                .output()?;
            require_success("rustc", &compile)?;
            send(sender, Event::Compiled(compile_started.elapsed()))?;
            (program.as_path(), "native-elf-v0")
        }
        Source::File(source) if source.extension().is_some_and(|value| value == "py") => {
            send(sender, Event::Phase("Preparing Python source".into()))?;
            (source.as_path(), "python3.12-source-v0")
        }
        Source::Python(source) => {
            send(sender, Event::Phase("Preparing Python source".into()))?;
            fs::write(&inline, source)?;
            (inline.as_path(), "python3.12-source-v0")
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "source must end in .rs or .py",
            ));
        }
    };
    let request = RunRequest::new(host, api_key, runtime, workload, instances)?;

    let execution = jio_client::run(&request, |event| match event {
        ClientEvent::Phase(phase) => send(sender, Event::Phase(phase)),
        ClientEvent::Done(_) => Ok(()),
        event => send(sender, Event::Client(event)),
    });
    execution?;
    Ok(())
}

fn temporary_directory() -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let path = env::temp_dir().join(format!("jio-cli-{}-{timestamp}", std::process::id()));
    fs::create_dir(&path)?;
    Ok(path)
}

fn send(sender: &Sender<Event>, event: Event) -> io::Result<()> {
    sender
        .send(event)
        .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "TUI closed"))
}

fn require_success(name: &str, output: &Output) -> io::Result<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{name} failed with {}\n{}{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )))
}
