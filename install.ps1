# Apache-2.0. Invoke from PowerShell, or powershell.exe -File install.ps1 in CMD.
# Invoke only after the complete script has downloaded.
function Install-Jio {
    $ErrorActionPreference = 'Stop'
    $version = '0.2.1'
    if ([Environment]::OSVersion.Platform -ne 'Win32NT') { throw 'Use install.sh on macOS or Linux.' }
    if ([Runtime.InteropServices.RuntimeInformation]::OSArchitecture -ne 'X64') {
        throw 'This release supports native Windows x64.'
    }
    foreach ($command in @('ssh.exe', 'ssh-keygen.exe')) {
        if (-not (Test-Path -LiteralPath (Join-Path $env:SystemRoot "System32\OpenSSH\$command") -PathType Leaf)) {
            throw 'Install Windows OpenSSH Client from Settings > Optional features, then rerun this installer.'
        }
    }
    $directory = $env:JIO_INSTALL_DIR
    if (-not $directory) { $directory = Join-Path $env:LOCALAPPDATA 'Jio\bin' }
    if (-not [IO.Path]::IsPathRooted($directory) -or $directory -notmatch '^[A-Za-z]:[\\/]') {
        throw 'JIO_INSTALL_DIR must be an absolute local Windows path.'
    }
    $directory = [IO.Path]::GetFullPath($directory)
    $destination = Join-Path $directory 'jio.exe'
    foreach ($path in @($directory, $destination)) {
        if (Test-Path -LiteralPath $path) {
            $item = Get-Item -LiteralPath $path -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Refusing reparse point: $path" }
        }
    }
    if ((Test-Path -LiteralPath $destination) -and -not (Test-Path -LiteralPath $destination -PathType Leaf)) {
        throw 'The existing jio.exe is not a regular file.'
    }
    [IO.Directory]::CreateDirectory($directory) | Out-Null
    $temporary = Join-Path ([IO.Path]::GetTempPath()) ('jio-install-' + [Guid]::NewGuid().ToString('N'))
    [IO.Directory]::CreateDirectory($temporary) | Out-Null
    $staged = $null
    try {
        $asset = 'jio-x86_64-pc-windows-msvc.zip'
        $release = "https://github.com/jiovannish/client/releases/download/v$version"
        Write-Host "Downloading Jio $version (Windows x64)..."
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -UseBasicParsing -Uri "$release/$asset" -OutFile "$temporary\archive.zip" -TimeoutSec 180
        Invoke-WebRequest -UseBasicParsing -Uri "$release/SHA256SUMS" -OutFile "$temporary\checksums" -TimeoutSec 30
        $checksumLines = @(Get-Content -LiteralPath "$temporary\checksums" | Where-Object { $_ -match ('^[0-9a-f]{64}  ' + [regex]::Escape($asset) + '$') })
        if ($checksumLines.Count -ne 1) { throw 'Missing or duplicate release checksum.' }
        $expected = $checksumLines[0].Substring(0, 64)
        if ((Get-FileHash -LiteralPath "$temporary\archive.zip" -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {
            throw 'Checksum mismatch; existing Jio was not changed.'
        }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $archive = [IO.Compression.ZipFile]::OpenRead("$temporary\archive.zip")
        try {
            # Read only fixed members, never extract arbitrary archive paths or links.
            foreach ($name in @('jio.exe', 'LICENSE', 'THIRDPARTY.json')) {
                $entries = @($archive.Entries | Where-Object { $_.FullName -ceq $name })
                if ($entries.Count -ne 1 -or $entries[0].Length -le 0 -or $entries[0].Length -gt 64MB) {
                    throw "Invalid release member: $name"
                }
                $inputStream = $entries[0].Open()
                $outputStream = [IO.File]::Create((Join-Path $temporary $name))
                try { $inputStream.CopyTo($outputStream) }
                finally { $outputStream.Dispose(); $inputStream.Dispose() }
            }
        } finally { $archive.Dispose() }
        $reported = & "$temporary\jio.exe" --version
        if ($LASTEXITCODE -ne 0 -or $reported -cne "jio $version") { throw 'Downloaded Jio failed its version check.' }
        $notices = Join-Path $directory '..\share\jio'
        [IO.Directory]::CreateDirectory($notices) | Out-Null
        Copy-Item -LiteralPath "$temporary\LICENSE", "$temporary\THIRDPARTY.json" -Destination $notices -Force
        # Stage on the destination volume. A running/locked binary fails safely.
        $staged = Join-Path $directory ('.jio-' + [Guid]::NewGuid().ToString('N') + '.exe')
        [IO.File]::Copy("$temporary\jio.exe", $staged, $false)
        if ([IO.File]::Exists($destination)) { [IO.File]::Replace($staged, $destination, $null) }
        else { [IO.File]::Move($staged, $destination) }
        $staged = $null
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        if ($directory -notin ($userPath -split ';')) {
            $newPath = if ($userPath) { $userPath.TrimEnd(';') + ';' + $directory } else { $directory }
            [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
        }
        if ($directory -notin ($env:Path -split ';')) { $env:Path = $directory + ';' + $env:Path }
        Write-Host "Installed Jio $version at $destination"
        Write-Host 'Open a new terminal to use jio from CMD or other existing terminals.'
        Write-Host 'Your API key, VM keys and saved configuration were not changed.'
    } finally {
        if ($staged -and (Test-Path -LiteralPath $staged)) { Remove-Item -LiteralPath $staged -Force }
        Remove-Item -LiteralPath $temporary -Recurse -Force
    }
}
Install-Jio
