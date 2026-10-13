# Install mushy and ln -m in the current user's PowerShell profile.
$ErrorActionPreference = 'Stop'
$version = if ($env:MUSHY_VERSION) { $env:MUSHY_VERSION } else { 'v0.1.1' }
$releaseBase = if ($env:MUSHY_RELEASE_BASE_URL) { $env:MUSHY_RELEASE_BASE_URL.TrimEnd('/') } else { "https://github.com/sudhanshug16/mushy-links/releases/download/$version" }
$installDir = if ($env:MUSHY_INSTALL_DIR) { $env:MUSHY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'mushy\bin' }
$selectedShell = if ($env:MUSHY_SHELL) { $env:MUSHY_SHELL } else { 'powershell' }
if ($selectedShell -notin @('bash', 'zsh', 'fish', 'powershell')) { throw 'Choose bash, zsh, fish, or powershell for MUSHY_SHELL.' }
if (!(Get-Command ssh -ErrorAction SilentlyContinue)) { throw 'OpenSSH is required. Install the SSH client, then run this installer again.' }
# Windows x64 binaries also run under Windows 11 ARM's x64 emulation.
if ($env:PROCESSOR_ARCHITECTURE -notin @('AMD64', 'ARM64') -and $env:PROCESSOR_ARCHITEW6432 -notin @('AMD64', 'ARM64')) { throw 'This release requires 64-bit Windows.' }
$asset = 'mushy-x86_64-pc-windows-msvc.zip'
$temp = Join-Path ([IO.Path]::GetTempPath()) ('mushy-install-' + [guid]::NewGuid())
[IO.Directory]::CreateDirectory($temp) | Out-Null
try {
    Write-Host "Installing mushy $version for Windows..."
    $archive = Join-Path $temp $asset
    Invoke-WebRequest -UseBasicParsing "$releaseBase/$asset" -OutFile $archive
    $response = Invoke-WebRequest -UseBasicParsing "$releaseBase/SHA256SUMS"
    $checksums = if ($response.Content -is [byte[]]) { [Text.Encoding]::UTF8.GetString($response.Content) } else { [string]$response.Content }
    $entries = @($checksums -split '\r?\n' | Where-Object { $_ -match ('^[0-9a-f]{64}  ' + [regex]::Escape($asset) + '$') })
    if ($entries.Count -ne 1) { throw 'The release checksum is missing or invalid.' }
    $expected = $entries[0].Substring(0, 64)
    $hasher = [Security.Cryptography.SHA256]::Create()
    $stream = [IO.File]::OpenRead($archive)
    try { $actual = [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
    finally { $stream.Dispose(); $hasher.Dispose() }
    if ($actual -ne $expected) { throw 'Download checksum mismatch; nothing was installed.' }
    $package = Join-Path $temp 'package'
    # Validate the archive's paths before extraction.
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        foreach ($entry in $zip.Entries) {
            if ($entry.FullName.Replace('\', '/') -notin @('mushy.exe', 'mushy-askpass.exe', 'mushy-background.exe', 'README.md', 'LICENSE', 'docs/', 'docs/reference.md')) { throw 'Unexpected release archive entry.' }
        }
    } finally { $zip.Dispose() }
    [IO.Compression.ZipFile]::ExtractToDirectory($archive, $package)
    $names = @('mushy.exe', 'mushy-askpass.exe', 'mushy-background.exe')
    foreach ($name in $names) { if (!(Test-Path -LiteralPath (Join-Path $package $name) -PathType Leaf)) { throw "The release is missing $name." } }
    $stagedMushy = Join-Path $package 'mushy.exe'
    $installedVersion = & $stagedMushy --version
    if ($LASTEXITCODE -ne 0 -or $installedVersion -ne ('mushy ' + $version.TrimStart('v'))) { throw 'Release version mismatch or incompatible Windows build; nothing was installed.' }
    [IO.Directory]::CreateDirectory($installDir) | Out-Null
    $installDir = [IO.Path]::GetFullPath($installDir)
    $mushy = Join-Path $installDir 'mushy.exe'
    $updating = Test-Path -LiteralPath $mushy
    if ($updating) {
        & $mushy daemon stop
        if ($LASTEXITCODE -ne 0) { throw 'Could not stop the existing mushy daemon.' }
        Start-Sleep -Seconds 1
    }
    foreach ($name in $names) { Copy-Item -LiteralPath (Join-Path $package $name) -Destination (Join-Path $installDir ('.' + $name + '.new')) -Force }
    foreach ($name in $names) { Move-Item -LiteralPath (Join-Path $installDir ('.' + $name + '.new')) -Destination (Join-Path $installDir $name) -Force }
    if ($env:MUSHY_NO_SHELL -ne '1') {
        $rc = if ($env:MUSHY_RC_FILE) { $env:MUSHY_RC_FILE } elseif ($selectedShell -eq 'powershell') { $PROFILE.CurrentUserAllHosts } else { $null }
        if ($rc) { & $mushy shell install $selectedShell --rc-file $rc } else { & $mushy shell install $selectedShell }
        if ($LASTEXITCODE -ne 0) { throw 'Installed mushy, but shell setup failed.' }
    }
    # A persistent user PATH also lets the remote SSH helper find mushy.
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (($userPath -split ';') -notcontains $installDir) { [Environment]::SetEnvironmentVariable('Path', ($installDir + ';' + $userPath), 'User') }
    if (($env:Path -split ';') -notcontains $installDir) { $env:Path = $installDir + ';' + $env:Path }
    if ($updating) { & $mushy daemon start; if ($LASTEXITCODE -ne 0) { throw 'Installed mushy, but could not restart the daemon.' } }
    if ($selectedShell -eq 'powershell' -and $env:MUSHY_NO_SHELL -ne '1') {
        $savedEncoding = [Console]::OutputEncoding
        try {
            [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)
            $setup = (& $mushy shell env powershell) | Out-String
            if ($LASTEXITCODE -ne 0) { throw 'Could not activate the shell shortcut.' }
        } finally { [Console]::OutputEncoding = $savedEncoding }
        Invoke-Expression $setup
    }
    Write-Host "`nInstalled mushy in $installDir"
    Write-Host 'Use: ln -m devbox:/home/me/notes ./notes'
    Write-Host 'Run this same installer on the other device too.'
} finally {
    Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue
}
