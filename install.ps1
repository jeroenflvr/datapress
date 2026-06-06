<#
.SYNOPSIS
    datapress installer for Windows.

.DESCRIPTION
    Downloads the prebuilt `datapress` CLI from the GitHub release, verifies
    its SHA-256 checksum, and installs it into a per-user directory (no admin
    rights). Adds that directory to your user PATH if it is not already there.

    Run it with:

        powershell -ExecutionPolicy ByPass -c "irm https://datap-rs.org/install.ps1 | iex"

.PARAMETER Version
    Version/tag to install (e.g. "0.4.4" or "v0.4.4"). Defaults to the latest
    release. Can also be set via $env:DATAPRESS_VERSION.

.PARAMETER InstallDir
    Directory to install into. Defaults to %LOCALAPPDATA%\datapress\bin.
    Can also be set via $env:DATAPRESS_INSTALL_DIR.

.PARAMETER NoModifyPath
    Do not add the install directory to the user PATH (just print a hint).
#>
[CmdletBinding()]
param(
    [string] $Version      = $env:DATAPRESS_VERSION,
    [string] $InstallDir   = $env:DATAPRESS_INSTALL_DIR,
    [switch] $NoModifyPath
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo    = 'jeroenflvr/datapress'
$BinName = 'datapress.exe'

function Write-Info  { param([string]$m) Write-Host $m }
function Write-Warn  { param([string]$m) Write-Host "warning: $m" -ForegroundColor Yellow }
function Die         { param([string]$m) Write-Host "error: $m" -ForegroundColor Red; exit 1 }

# ---- detect target -------------------------------------------------------
$arch = $env:PROCESSOR_ARCHITECTURE
switch ($arch) {
    'AMD64' { $target = 'x86_64-pc-windows-msvc' }
    'x86'   { $target = 'x86_64-pc-windows-msvc' }   # run the x64 build under WOW64
    default { Die "unsupported architecture: $arch. Try: cargo install datapress" }
}
Write-Info "Detected platform: $target"

# ---- resolve install dir -------------------------------------------------
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
    $InstallDir = Join-Path $env:LOCALAPPDATA 'datapress\bin'
}

# ---- resolve version -----------------------------------------------------
function Get-LatestTag {
    # Follow the /releases/latest redirect; the final URL ends with the tag.
    $resp = Invoke-WebRequest -Uri "https://github.com/$Repo/releases/latest" `
        -MaximumRedirection 5 -UseBasicParsing
    $final = $resp.BaseResponse.ResponseUri.AbsoluteUri
    return ($final -split '/')[-1]
}

if ([string]::IsNullOrWhiteSpace($Version)) {
    $tag = Get-LatestTag
    if ([string]::IsNullOrWhiteSpace($tag)) { Die "could not determine the latest version; pass -Version" }
} elseif ($Version.StartsWith('v')) {
    $tag = $Version
} else {
    $tag = "v$Version"
}
Write-Info "Installing datapress $tag"

# ---- download + verify + extract ----------------------------------------
$archive = "datapress-$tag-$target.zip"
$baseUrl = "https://github.com/$Repo/releases/download/$tag"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("datapress-" + [System.Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
try {
    $zipPath = Join-Path $tmp $archive
    Write-Info "Downloading $archive"
    Invoke-WebRequest -Uri "$baseUrl/$archive" -OutFile $zipPath -UseBasicParsing

    # Optional checksum verification.
    $sumPath = "$zipPath.sha256"
    try {
        Invoke-WebRequest -Uri "$baseUrl/$archive.sha256" -OutFile $sumPath -UseBasicParsing -ErrorAction Stop
        $expected = ((Get-Content $sumPath -Raw).Trim() -split '\s+')[0]
        $actual   = (Get-FileHash -Algorithm SHA256 -Path $zipPath).Hash.ToLower()
        if ($expected.ToLower() -ne $actual) {
            Die "checksum mismatch: expected $expected, got $actual"
        }
        Write-Info "Checksum OK"
    } catch {
        Write-Warn "checksum file not published for this release; skipping verification"
    }

    Expand-Archive -Path $zipPath -DestinationPath $tmp -Force
    $extracted = Join-Path $tmp $BinName
    if (-not (Test-Path $extracted)) { Die "archive did not contain '$BinName'" }

    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
    $dest = Join-Path $InstallDir $BinName
    Copy-Item -Path $extracted -Destination $dest -Force

    Write-Info ""
    Write-Info "Installed datapress to $dest"
}
finally {
    Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
}

# ---- PATH handling -------------------------------------------------------
$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$onPath = ($userPath -split ';') -contains $InstallDir

if ($onPath) {
    Write-Info "$InstallDir is already on your PATH. Run: datapress --version"
}
elseif ($NoModifyPath) {
    Write-Warn "$InstallDir is not on your PATH."
    Write-Info "Add it manually, or re-run without -NoModifyPath."
}
else {
    $newPath = if ([string]::IsNullOrEmpty($userPath)) { $InstallDir } else { "$userPath;$InstallDir" }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    # Update the current session too.
    $env:Path = "$env:Path;$InstallDir"
    Write-Info ""
    Write-Info "Added $InstallDir to your user PATH."
    Write-Info "Open a new terminal (or restart it) and run: datapress --version"
}
