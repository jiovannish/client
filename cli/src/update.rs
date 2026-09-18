use std::io;
use std::path::Path;
use std::process::Command;

pub fn run() -> io::Result<()> {
    let executable = std::env::current_exe()?;
    let mut command = installer(&executable)?;
    println!("Updating Jio to the latest release...");
    let status = command.status()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "update failed ({status}); check the installer output above"
        )));
    }
    Ok(())
}

fn installer(executable: &Path) -> io::Result<Command> {
    let name = if cfg!(windows) { "jio.exe" } else { "jio" };
    if executable.file_name() != Some(std::ffi::OsStr::new(name)) {
        return Err(io::Error::other(format!(
            "cannot update a renamed executable; reinstall Jio as {name}"
        )));
    }
    let directory = executable
        .parent()
        .ok_or_else(|| io::Error::other("cannot determine the Jio installation directory"))?;
    #[cfg(unix)]
    let mut command = {
        let mut command = Command::new("sh");
        command.args(["-c", r#"set -eu
installer=$(curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 30 https://github.com/jiovannish/client/releases/latest/download/install.sh)
test -n "$installer"
printf '%s\n' "$installer" | sh
"#]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let powershell = std::path::PathBuf::from(
            std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()),
        )
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let mut command = Command::new(powershell);
        command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", r#"$ErrorActionPreference = 'Stop'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
$installer = (Invoke-WebRequest -UseBasicParsing -Uri 'https://github.com/jiovannish/client/releases/latest/download/install.ps1' -TimeoutSec 30).Content
if (-not $installer) { throw 'Empty installer response' }
& ([ScriptBlock]::Create($installer))
"#]);
        command
    };
    // Update the executable actually invoked, even with a custom install or PATH.
    // Pass the path as data, never as shell source. No account setup is involved.
    command.env("JIO_INSTALL_DIR", directory);
    Ok(command)
}
