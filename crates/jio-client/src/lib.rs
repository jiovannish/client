use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_INSTANCES: usize = 64;
const MAX_ERROR_BYTES: u64 = 64 * 1024;
const REMOTE_RUNNER: &[u8] = include_bytes!("remote-run.sh");

pub struct RunRequest {
    host: String,
    workload: PathBuf,
    instances: usize,
}

impl RunRequest {
    pub fn new(
        host: impl Into<String>,
        workload: impl Into<PathBuf>,
        instances: usize,
    ) -> io::Result<Self> {
        let host = normalize_host(host.into())?;
        if !(1..=MAX_INSTANCES).contains(&instances) {
            return Err(invalid(format!(
                "instance count must be between 1 and {MAX_INSTANCES}"
            )));
        }
        Ok(Self {
            host,
            workload: workload.into(),
            instances,
        })
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn instances(&self) -> usize {
        self.instances
    }
}

#[derive(Clone)]
pub struct VmResult {
    pub index: usize,
    pub raw_fork: Duration,
    pub ready: Duration,
    pub workload_send: Duration,
    pub program_complete: Duration,
    pub teardown: Duration,
    pub output: String,
}

pub enum Event {
    Phase(String),
    Uploaded(Duration),
    WorkloadLoaded(Duration),
    TemplateLoaded(Duration),
    Vm(VmResult),
    Done(Duration),
}

pub fn run(request: &RunRequest, mut emit: impl FnMut(Event) -> io::Result<()>) -> io::Result<()> {
    if !request.workload.is_file() {
        return Err(invalid(format!(
            "workload is not a file: {}",
            request.workload.display()
        )));
    }

    let started = Instant::now();
    let remote_workload = remote_workload_path()?;
    emit(Event::Phase(format!("Uploading ELF to {}", request.host)))?;
    let upload_started = Instant::now();
    if let Err(error) = upload(&request.host, &request.workload, &remote_workload) {
        remove_remote(&request.host, &remote_workload);
        return Err(error);
    }
    if let Err(error) = emit(Event::Uploaded(upload_started.elapsed())) {
        remove_remote(&request.host, &remote_workload);
        return Err(error);
    }

    emit(Event::Phase("Restoring existing template".into()))?;
    let result = execute_remote(request, &remote_workload, &mut emit);
    if result.is_err() {
        remove_remote(&request.host, &remote_workload);
    }
    result?;
    emit(Event::Done(started.elapsed()))
}

fn execute_remote(
    request: &RunRequest,
    remote_workload: &str,
    emit: &mut impl FnMut(Event) -> io::Result<()>,
) -> io::Result<()> {
    let mut remote = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg(&request.host)
        .arg("bash")
        .arg("-s")
        .arg("--")
        .arg(remote_workload)
        .arg(request.instances.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let mut stdin = remote
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("ssh stdin was unavailable"))?;
    stdin.write_all(REMOTE_RUNNER)?;
    drop(stdin);

    let stdout = remote
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("ssh stdout was unavailable"))?;
    let mut stderr = remote
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("ssh stderr was unavailable"))?;
    let error_reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr
            .by_ref()
            .take(MAX_ERROR_BYTES)
            .read_to_end(&mut bytes)
            .map(|_| bytes)
    });

    for line in BufReader::new(stdout).lines() {
        parse(&line?, emit)?;
    }

    let status = remote.wait()?;
    let stderr = match error_reader.join() {
        Ok(result) => result?,
        Err(_) => return Err(io::Error::other("ssh stderr reader panicked")),
    };
    if status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "remote runner failed with {status}\n{}",
        String::from_utf8_lossy(&stderr)
    )))
}

fn upload(host: &str, source: &Path, remote: &str) -> io::Result<()> {
    let destination = format!("{host}:{remote}");
    let output = Command::new("scp")
        .arg("-q")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg(source)
        .arg(destination)
        .output()?;
    require_success("scp", &output)
}

fn remove_remote(host: &str, remote: &str) {
    let _ = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg(host)
        .arg("rm")
        .arg("-f")
        .arg("--")
        .arg(remote)
        .status();
}

fn parse(line: &str, emit: &mut impl FnMut(Event) -> io::Result<()>) -> io::Result<()> {
    if let Some(value) = line.strip_prefix("workload_load_ns=") {
        let value = value
            .split_whitespace()
            .next()
            .ok_or_else(|| invalid("Core workload timing was empty"))?;
        return emit(Event::WorkloadLoaded(nanos(value)?));
    }
    if let Some(value) = line.strip_prefix("template_load_ns=") {
        return emit(Event::TemplateLoaded(nanos(value)?));
    }
    if line.starts_with("vm=") {
        return emit(Event::Vm(parse_vm(line)?));
    }
    Ok(())
}

fn parse_vm(line: &str) -> io::Result<VmResult> {
    let (metrics, output) = line
        .split_once(" output=")
        .ok_or_else(|| invalid("Core VM result had no output field"))?;
    let mut fields = metrics.split_whitespace();
    let index = number(metric(fields.next(), "vm=")?)?;
    let raw_fork = nanos(metric(fields.next(), "raw_fork_ns=")?)?;
    let ready = nanos(metric(fields.next(), "guest_ready_ns=")?)?;
    let workload_send = nanos(metric(fields.next(), "workload_send_ns=")?)?;
    let program_complete = nanos(metric(fields.next(), "program_complete_ns=")?)?;
    let teardown = nanos(metric(fields.next(), "teardown_ns=")?)?;
    if fields.next().is_some() {
        return Err(invalid("Core VM result had unexpected metrics"));
    }
    let output = output
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .ok_or_else(|| invalid("Core VM output was not quoted"))?;
    Ok(VmResult {
        index,
        raw_fork,
        ready,
        workload_send,
        program_complete,
        teardown,
        output: output.into(),
    })
}

fn metric<'a>(field: Option<&'a str>, prefix: &str) -> io::Result<&'a str> {
    field
        .and_then(|value| value.strip_prefix(prefix))
        .ok_or_else(|| invalid(format!("Core VM result lacked {prefix}")))
}

fn nanos(value: &str) -> io::Result<Duration> {
    value.parse().map(Duration::from_nanos).map_err(invalid)
}

fn number(value: &str) -> io::Result<usize> {
    value.parse().map_err(invalid)
}

fn remote_workload_path() -> io::Result<String> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    Ok(format!(
        "/tmp/jio-client-{}-{timestamp}.elf",
        std::process::id()
    ))
}

fn normalize_host(host: String) -> io::Result<String> {
    if host.is_empty() || host.bytes().filter(|byte| *byte == b'@').count() > 1 {
        return Err(invalid("host is invalid"));
    }
    let host = if host.contains('@') {
        host
    } else {
        format!("ubuntu@{host}")
    };
    let (user, address) = host
        .split_once('@')
        .ok_or_else(|| invalid("host is invalid"))?;
    if user.is_empty()
        || address.is_empty()
        || !host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".@:_-[]".contains(&byte))
    {
        return Err(invalid("host contains unsupported characters"));
    }
    Ok(host)
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

fn invalid(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::{Event, RunRequest, parse};
    use std::io;
    use std::path::PathBuf;

    #[test]
    fn normalizes_a_bare_host() -> io::Result<()> {
        let request = RunRequest::new("192.168.1.81", PathBuf::from("program"), 5)?;
        assert_eq!(request.host(), "ubuntu@192.168.1.81");
        assert_eq!(request.instances(), 5);
        Ok(())
    }

    #[test]
    fn rejects_shell_characters_in_a_host() {
        assert!(RunRequest::new("host;reboot", "program", 1).is_err());
    }

    #[test]
    fn parses_a_vm_result() -> io::Result<()> {
        let mut result = None;
        parse(
            "vm=2 raw_fork_ns=1 guest_ready_ns=2 workload_send_ns=3 program_complete_ns=4 teardown_ns=5 output=\"ok\"",
            &mut |event| {
                if let Event::Vm(vm) = event {
                    result = Some(vm);
                }
                Ok(())
            },
        )?;
        let vm = result.ok_or_else(|| io::Error::other("missing VM result"))?;
        assert_eq!(vm.index, 2);
        assert_eq!(vm.output, "ok");
        Ok(())
    }
}
