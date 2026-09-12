# Run on Windows after the native release binary is built. No GitHub downloads.
$ErrorActionPreference = 'Stop'
$installer = Join-Path $PSScriptRoot '..\install.ps1'
$binary = Join-Path $PSScriptRoot '..\target\x86_64-pc-windows-msvc\release\jio.exe'
# The build runner has Visual C++; users should not need its runtime DLLs.
$vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$dumpbin = & $vswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -find 'VC\Tools\MSVC\**\bin\Hostx64\x64\dumpbin.exe' | Select-Object -First 1
if (-not $dumpbin) { throw 'Cannot inspect Windows executable dependencies' }
$dependencies = & $dumpbin /dependents $binary
if ($LASTEXITCODE -ne 0 -or ($dependencies -match '(VCRUNTIME|MSVCP)[0-9].*\.dll')) {
    throw 'Windows executable must not require the Visual C++ redistributable'
}
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('jio-test-' + [Guid]::NewGuid().ToString('N'))
$originalDirectory = $env:JIO_INSTALL_DIR
$originalPath = $env:Path
$originalUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$badChecksum = $false
$downloadFailure = $false
function Invoke-WebRequest {
    param($Uri, $OutFile, $TimeoutSec, [switch]$UseBasicParsing)
    if ($downloadFailure) { throw 'simulated download failure' }
    if ($Uri.EndsWith('/SHA256SUMS')) {
        $hash = if ($badChecksum) { '0' * 64 } else { (Get-FileHash "$fixture\release.zip").Hash.ToLowerInvariant() }
        [IO.File]::WriteAllText($OutFile, "$hash  jio-x86_64-pc-windows-msvc.zip`n")
    } elseif ($Uri.EndsWith('/jio-x86_64-pc-windows-msvc.zip')) {
        Copy-Item -LiteralPath "$fixture\release.zip" -Destination $OutFile
    } else { throw "Unexpected URL: $Uri" }
}
try {
    [IO.Directory]::CreateDirectory("$fixture\release") | Out-Null
    Copy-Item -LiteralPath $binary -Destination "$fixture\release\jio.exe"
    [IO.File]::WriteAllText("$fixture\release\LICENSE", 'fixture license')
    [IO.File]::WriteAllText("$fixture\release\THIRDPARTY.json", '{}')
    Compress-Archive -Path "$fixture\release\*" -DestinationPath "$fixture\release.zip"
    $env:JIO_INSTALL_DIR = "$fixture\directory with spaces\bin"
    & $installer
    & $installer # Updating an existing install is supported.
    $installed = "$env:JIO_INSTALL_DIR\jio.exe"
    if ((& $installed --version) -ne 'jio 0.2.1') { throw 'Installed version mismatch' }
    $before = (Get-FileHash $installed).Hash
    foreach ($failure in @('checksum', 'download')) {
        $badChecksum = $failure -eq 'checksum'
        $downloadFailure = $failure -eq 'download'
        $failed = $false
        try { & $installer } catch { $failed = $true }
        if (-not $failed) { throw "Expected $failure failure" }
        if ((Get-FileHash $installed).Hash -ne $before) { throw 'Failed install changed the binary' }
    }
    $env:JIO_TEST_BINARY = $installed
    $version = & powershell.exe -NoProfile -NonInteractive -Command '& $env:JIO_TEST_BINARY --version'
    if ($LASTEXITCODE -ne 0 -or $version -ne 'jio 0.2.1') { throw 'PowerShell invocation failed' }
    $version = & cmd.exe /d /c '"%JIO_TEST_BINARY%" --version'
    if ($LASTEXITCODE -ne 0 -or $version -ne 'jio 0.2.1') { throw 'CMD invocation failed' }
    Write-Host 'PASS: native PowerShell/CMD, paths with spaces, updates, checksum and download failures'
} finally {
    $env:JIO_INSTALL_DIR = $originalDirectory
    $env:Path = $originalPath
    [Environment]::SetEnvironmentVariable('Path', $originalUserPath, 'User')
    Remove-Item Env:JIO_TEST_BINARY -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
