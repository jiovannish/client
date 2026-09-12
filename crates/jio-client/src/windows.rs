//! Windows credential storage. Files inherit a protected, user-owned directory ACL.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub(crate) fn system_executable(relative: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into()))
        .join("System32")
        .join(relative)
}

const REPARSE_POINT: u32 = 0x400;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

fn acl(path: &Path, create: bool) -> io::Result<()> {
    // Pass paths as data, never interpolate them into PowerShell source.
    // Only OS administrators, SYSTEM and the current user may have access.
    let script = r#"$ErrorActionPreference = 'Stop'
$p = $env:JIO_ACL_PATH
$sid = [Security.Principal.WindowsIdentity]::GetCurrent().User
if ($env:JIO_ACL_CREATE -eq '1') {
    $acl = New-Object Security.AccessControl.DirectorySecurity
    $acl.SetOwner($sid)
    $acl.SetAccessRuleProtection($true, $false)
    $rule = New-Object Security.AccessControl.FileSystemAccessRule($sid, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
    $acl.AddAccessRule($rule)
    Set-Acl -LiteralPath $p -AclObject $acl
}
$acl = Get-Acl -LiteralPath $p
$rules = $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])
$allowed = @($sid.Value, 'S-1-5-18', 'S-1-5-32-544')
if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -notin $allowed) { throw 'Credential path has a different owner' }
if ($rules.Count -eq 0) { throw 'Credential path has no restrictive access rules' }
foreach ($rule in $rules) {
    if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -notin $allowed) {
        throw 'Credential path permits access by another user'
    }
}
"#;
    let status = Command::new(system_executable("WindowsPowerShell\\v1.0\\powershell.exe"))
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .env("JIO_ACL_PATH", path)
        .env("JIO_ACL_CREATE", if create { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "cannot verify private Windows permissions: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn regular(path: &Path, directory: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_attributes() & REPARSE_POINT != 0
        || (directory && !metadata.is_dir())
        || (!directory && !metadata.is_file())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "credential path must be a regular file or directory, not a reparse point",
        ));
    }
    Ok(())
}

/// Create a new directory and restrict its ACL before any secrets are written.
pub fn new_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)?;
    if let Err(error) = acl(path, true) {
        let _ = fs::remove_dir(path);
        return Err(error);
    }
    require_directory(path)
}

/// Existing directories must already be private; do not silently change their ACLs.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => require_directory(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => new_private_directory(path),
        Err(error) => Err(error),
    }
}

/// Validate a private state directory, rejecting junctions and symlinks.
pub fn require_directory(path: &Path) -> io::Result<()> {
    regular(path, true)?;
    acl(path, false)
}

/// Validate an existing private credential file.
pub fn require_private_file(path: &Path) -> io::Result<()> {
    regular(path, false)?;
    acl(path, false)
}

/// Lock the file against replacement/writes while checking its ACL and reading it.
pub fn open_private_read(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .share_mode(1)
        .custom_flags(OPEN_REPARSE_POINT)
        .open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.file_attributes() & REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "invalid credential file",
        ));
    }
    require_private_file(path)?;
    Ok(file)
}

/// Create a file under a checked directory; the private ACL is inherited at creation.
pub fn new_private_file(path: &Path) -> io::Result<File> {
    require_directory(
        path.parent()
            .ok_or_else(|| io::Error::other("missing parent"))?,
    )?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(1)
        .open(path)?;
    require_private_file(path)?;
    Ok(file)
}

/// Persist a new private file without overwriting existing authority.
pub fn write_private_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = new_private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
#[test]
fn credentials_are_private_and_locked_during_reads() -> io::Result<()> {
    let root = super::vm::temporary_directory(&std::env::temp_dir())?;
    let path = root.join("credential");
    let result = (|| -> io::Result<()> {
        write_private_file(&path, b"test credential")?;
        let file = open_private_read(&path)?;
        assert!(fs::rename(&path, root.join("moved")).is_err());
        drop(file);
        let status = Command::new(system_executable("WindowsPowerShell\\v1.0\\powershell.exe"))
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                r#"
$ErrorActionPreference='Stop'
$acl=Get-Acl -LiteralPath $env:JIO_ACL_PATH
$sid=New-Object Security.Principal.SecurityIdentifier('S-1-1-0')
$rule=New-Object Security.AccessControl.FileSystemAccessRule($sid,'Read','Allow')
$acl.AddAccessRule($rule)
Set-Acl -LiteralPath $env:JIO_ACL_PATH -AclObject $acl
"#,
            ])
            .env("JIO_ACL_PATH", &path)
            .status()?;
        assert!(status.success());
        assert!(open_private_read(&path).is_err());
        Ok(())
    })();
    fs::remove_dir_all(root)?;
    result
}
