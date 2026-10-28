# Installs omoi on Windows and sets up Omoikane in Claude Code.
#   irm https://raw.githubusercontent.com/marcmayol/omoikane/master/install.ps1 | iex
#
# Optional variables:
#   OMOIKANE_INSTALL_DIR   where the binary goes (default %LOCALAPPDATA%\Programs\omoikane)
#   OMOIKANE_VERSION       a specific version, e.g. v0.1.0 (default: the latest)
#   OMOIKANE_NO_MODEL=1    don't download the model (~150 MB; without it, search by words only)
#   OMOIKANE_NO_SETUP=1    don't touch Claude Code
#   OMOIKANE_NO_PATH=1     don't add the folder to the user PATH
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'  # without the progress bar, Invoke-WebRequest is much faster on PowerShell 5.1

$repo = 'marcmayol/omoikane'
$dir = if ($env:OMOIKANE_INSTALL_DIR) { $env:OMOIKANE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\omoikane' }
$base = if ($env:OMOIKANE_VERSION) { "https://github.com/$repo/releases/download/$($env:OMOIKANE_VERSION)" } else { "https://github.com/$repo/releases/latest/download" }
# Windows on ARM runs the x64 binary through emulation.
$paquete = 'omoi-x86_64-pc-windows-msvc.zip'

$tmp = Join-Path ([IO.Path]::GetTempPath()) ("omoikane-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Write-Host "Downloading $paquete..."
    Invoke-WebRequest -UseBasicParsing "$base/$paquete" -OutFile (Join-Path $tmp $paquete)
    Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')

    # The checksum must match before anything is installed.
    $linea = Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object { $_ -match " $([regex]::Escape($paquete))$" }
    $esperada = if ($linea) { ($linea -split ' ')[0] } else { '' }
    $real = (Get-FileHash (Join-Path $tmp $paquete) -Algorithm SHA256).Hash.ToLower()
    if (-not $esperada -or $esperada -ne $real) { throw "the SHA-256 checksum of $paquete doesn't match; nothing was installed" }

    Expand-Archive (Join-Path $tmp $paquete) -DestinationPath $tmp -Force
    New-Item -ItemType Directory -Path $dir -Force | Out-Null
    $exe = Join-Path $dir 'omoi.exe'
    try {
        Copy-Item (Join-Path $tmp 'omoi.exe') $exe -Force
    } catch {
        throw "can't replace ${exe}: an open Claude Code session is probably using it. Close them and run the installer again."
    }
    Write-Host "Installed: $exe ($(& $exe --version))"

    if (-not $env:OMOIKANE_NO_PATH) {
        $usuario = [Environment]::GetEnvironmentVariable('Path', 'User')
        if (-not (($usuario -split ';') -contains $dir)) {
            [Environment]::SetEnvironmentVariable('Path', ($(if ($usuario) { "$usuario;" }) + $dir), 'User')
            Write-Host "Added to your user PATH: $dir (new terminals will see it)."
        }
        if (-not (($env:Path -split ';') -contains $dir)) { $env:Path = "$env:Path;$dir" }
    }

    if (-not $env:OMOIKANE_NO_MODEL) {
        & $exe model install
        if ($LASTEXITCODE -ne 0) { Write-Warning "couldn't install the model; try again with: omoi model install" }
    }
    if (-not $env:OMOIKANE_NO_SETUP) {
        & $exe setup claude --yes
        if ($LASTEXITCODE -ne 0) { Write-Warning "Claude Code isn't set up yet; once you have it, run: omoi setup claude" }
    }
} finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}
