# Remove mushy from the current user, retaining linked files and history.
$ErrorActionPreference = 'Stop'
$installDir = if ($env:MUSHY_INSTALL_DIR) { $env:MUSHY_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'mushy\bin' }
$installDir = [IO.Path]::GetFullPath($installDir)
$mushy = Join-Path $installDir 'mushy.exe'
if (!(Test-Path -LiteralPath $mushy -PathType Leaf)) { throw "No executable at $mushy. Set MUSHY_INSTALL_DIR for a custom installation." }
$temp = $null
try {
    $helper = $mushy
    if ((& $mushy daemon --help | Out-String) -notmatch '\buninstall\b') {
        Write-Host 'Fetching a cleanup helper for this older installation...'
        $temp = Join-Path ([IO.Path]::GetTempPath()) ('mushy-uninstall-' + [guid]::NewGuid())
        [IO.Directory]::CreateDirectory($temp) | Out-Null
        $releaseBase = if ($env:MUSHY_RELEASE_BASE_URL) { $env:MUSHY_RELEASE_BASE_URL.TrimEnd('/') } else { 'https://github.com/sudhanshug16/mushy-links/releases/download/v0.1.2' }
        $asset = 'mushy-x86_64-pc-windows-msvc.zip'
        $archive = Join-Path $temp $asset
        Invoke-WebRequest -UseBasicParsing ($releaseBase + '/' + $asset) -OutFile $archive
        $response = Invoke-WebRequest -UseBasicParsing ($releaseBase + '/SHA256SUMS')
        $checksums = if ($response.Content -is [byte[]]) { [Text.Encoding]::UTF8.GetString($response.Content) } else { [string]$response.Content }
        $entries = @($checksums -split '\r?\n' | Where-Object { $_ -match ('^[0-9a-f]{64}  ' + [regex]::Escape($asset) + '$') })
        if ($entries.Count -ne 1) { throw 'The cleanup helper checksum is missing or invalid.' }
        $hasher = [Security.Cryptography.SHA256]::Create()
        $stream = [IO.File]::OpenRead($archive)
        try { $actual = [BitConverter]::ToString($hasher.ComputeHash($stream)).Replace('-', '').ToLowerInvariant() }
        finally { $stream.Dispose(); $hasher.Dispose() }
        if ($actual -ne $entries[0].Substring(0, 64)) { throw 'Cleanup helper checksum mismatch; executables retained.' }
        Add-Type -AssemblyName System.IO.Compression.FileSystem
        $helper = Join-Path $temp 'mushy.exe'
        $zip = [IO.Compression.ZipFile]::OpenRead($archive)
        try {
            $entry = $zip.GetEntry('mushy.exe')
            if (!$entry) { throw 'The cleanup helper is missing.' }
            $inputStream = $entry.Open()
            $outputStream = [IO.File]::Create($helper)
            try { $inputStream.CopyTo($outputStream) }
            finally { $inputStream.Dispose(); $outputStream.Dispose() }
        } finally { $zip.Dispose() }
        if ((& $helper --version) -ne 'mushy 0.1.2' -or $LASTEXITCODE -ne 0) { throw 'Cleanup helper version mismatch; executables retained.' }
    }
    & $helper daemon uninstall
    if ($LASTEXITCODE -ne 0) { throw 'Could not remove startup or stop syncing; executables retained.' }
    if ($env:MUSHY_RC_FILE) {
        $selectedShell = if ($env:MUSHY_SHELL) { $env:MUSHY_SHELL } else { 'powershell' }
        & $helper shell uninstall $selectedShell --rc-file $env:MUSHY_RC_FILE
    } else {
        & $helper shell uninstall
    }
    if ($LASTEXITCODE -ne 0) { throw 'Could not remove shell setup; executables retained.' }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    $kept = @($userPath -split ';' | Where-Object { $_.TrimEnd('\', '/') -ine $installDir.TrimEnd('\', '/') })
    if (($kept -join ';') -ne $userPath) { [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User') }
    $env:Path = (@($env:Path -split ';' | Where-Object { $_.TrimEnd('\', '/') -ine $installDir.TrimEnd('\', '/') }) -join ';')
    foreach ($name in @('mushy.exe', 'mushy-askpass.exe', 'mushy-background.exe')) {
        $path = Join-Path $installDir $name
        if (Test-Path -LiteralPath $path -PathType Leaf) { Remove-Item -LiteralPath $path -Force }
    }
    # In a piped invocation, also drop the shortcut from this PowerShell session.
    Remove-Item Function:ln -ErrorAction SilentlyContinue
    Write-Host 'Uninstalled mushy. Linked files, sync state, recovery history, and saved passwords were kept.'
    Write-Host 'Open a new terminal to restore your normal shell setup.'
} finally {
    if ($temp) { Remove-Item -LiteralPath $temp -Recurse -Force -ErrorAction SilentlyContinue }
}
