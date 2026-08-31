<#
.SYNOPSIS
    Sets up the Windows environment and builds Simple RTSP Viewer.

.DESCRIPTION
    Runs, end to end and from PowerShell:
      1. Installs the build tools (Rust, LLVM, Visual Studio Build Tools) with winget.
      2. Downloads and extracts the FFmpeg libraries ("shared" build with headers/libs/DLLs).
      3. Sets the required environment variables (FFMPEG_DIR, LIBCLANG_PATH, PATH),
         permanently (user level) and also in the current session.
      4. Builds the project in release mode.

    Run it from the project folder:
        powershell -ExecutionPolicy Bypass -File .\setup.ps1

.PARAMETER FfmpegDir
    FFmpeg destination folder. Default: C:\ffmpeg

.PARAMETER SkipInstall
    Skips installing the tools with winget (use it if everything is already installed).

.PARAMETER SkipBuild
    Skips the 'cargo build --release' step.

.PARAMETER Force
    Reinstalls/downloads FFmpeg again even if the destination folder already exists.
#>

[CmdletBinding()]
param(
    [string]$FfmpegDir = "C:\ffmpeg",
    [switch]$SkipInstall,
    [switch]$SkipBuild,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

function Write-Step($msg) { Write-Host "`n==> $msg" -ForegroundColor Cyan }
function Write-Ok($msg)   { Write-Host "    $msg" -ForegroundColor Green }
function Write-Warn2($msg) { Write-Host "    $msg" -ForegroundColor Yellow }

# Reloads PATH (Machine + User) in the current session, so newly installed
# programs are found without reopening the terminal.
function Update-SessionPath {
    $machine = [Environment]::GetEnvironmentVariable("Path", "Machine")
    $user    = [Environment]::GetEnvironmentVariable("Path", "User")
    $env:Path = "$machine;$user"
}

# ---------------------------------------------------------------------------
# 1. Build tools
# ---------------------------------------------------------------------------
if (-not $SkipInstall) {
    Write-Step "Installing build tools with winget"

    if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
        throw "winget not found. Install 'App Installer' from the Microsoft Store and run again."
    }

    $wingetCommon = @(
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--silent",
        "-e"
    )

    function Install-WingetPackage($id, $extraArgs) {
        Write-Host "    - $id"
        $args = @("install", "--id", $id) + $wingetCommon
        if ($extraArgs) { $args += $extraArgs }
        winget @args
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne -1978335189) {
            # -1978335189 = "already installed / no applicable update"
            Write-Warn2 "winget returned code $LASTEXITCODE for $id (it may already be installed)."
        }
    }

    Install-WingetPackage "Rustlang.Rustup"
    Install-WingetPackage "LLVM.LLVM"
    Install-WingetPackage "Microsoft.VisualStudio.2022.BuildTools" `
        @("--override", "--wait --quiet --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended")

    Update-SessionPath

    # Makes sure the Rust MSVC toolchain is set.
    if (Get-Command rustup -ErrorAction SilentlyContinue) {
        rustup default stable-msvc | Out-Null
        Write-Ok "Rust configured (stable-msvc)."
    }
    else {
        Write-Warn2 "rustup is not on PATH yet; reopen PowerShell and run 'rustup default stable-msvc'."
    }
}
else {
    Write-Step "Install step skipped (-SkipInstall)"
}

# ---------------------------------------------------------------------------
# 2. FFmpeg (headers + libs + DLLs)
# ---------------------------------------------------------------------------
$ffLib = Join-Path $FfmpegDir "lib"
$ffInc = Join-Path $FfmpegDir "include"
$ffBin = Join-Path $FfmpegDir "bin"

if ($Force -and (Test-Path $FfmpegDir)) {
    Write-Warn2 "Removing $FfmpegDir (-Force)."
    Remove-Item $FfmpegDir -Recurse -Force
}

if ((Test-Path $ffLib) -and (Test-Path $ffInc)) {
    Write-Step "FFmpeg already present in $FfmpegDir (use -Force to download it again)"
}
else {
    Write-Step "Downloading and extracting FFmpeg (shared build)"
    $ffUrl = "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl-shared.zip"
    $zip   = Join-Path $env:TEMP "ffmpeg-shared.zip"
    $tmp   = Join-Path $env:TEMP "ffmpeg-extract"

    Write-Host "    Downloading: $ffUrl"
    Invoke-WebRequest -Uri $ffUrl -OutFile $zip

    if (Test-Path $tmp) { Remove-Item $tmp -Recurse -Force }
    Write-Host "    Extracting..."
    Expand-Archive -Path $zip -DestinationPath $tmp -Force

    # The zip contains a single root folder with bin/include/lib.
    $inner = Get-ChildItem $tmp -Directory | Select-Object -First 1
    if (-not $inner) { throw "Could not find the extracted FFmpeg folder." }

    New-Item -ItemType Directory -Path $FfmpegDir -Force | Out-Null
    Copy-Item -Path (Join-Path $inner.FullName "*") -Destination $FfmpegDir -Recurse -Force

    Remove-Item $zip -Force -ErrorAction SilentlyContinue
    Remove-Item $tmp -Recurse -Force -ErrorAction SilentlyContinue

    if (-not (Test-Path $ffLib)) { throw "FFmpeg extracted, but 'lib' was not found in $FfmpegDir." }
    Write-Ok "FFmpeg installed in $FfmpegDir."
}

# ---------------------------------------------------------------------------
# 3. Environment variables
# ---------------------------------------------------------------------------
Write-Step "Setting environment variables"

$llvmBin = "C:\Program Files\LLVM\bin"

# Permanent (user level).
[Environment]::SetEnvironmentVariable("FFMPEG_DIR", $FfmpegDir, "User")
[Environment]::SetEnvironmentVariable("LIBCLANG_PATH", $llvmBin, "User")

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$ffBin*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$ffBin", "User")
    Write-Ok "Added $ffBin to the user PATH."
}

# Current session (so the build right below works without reopening the terminal).
$env:FFMPEG_DIR    = $FfmpegDir
$env:LIBCLANG_PATH = $llvmBin
if ($env:Path -notlike "*$ffBin*") { $env:Path += ";$ffBin" }

Write-Ok "FFMPEG_DIR    = $env:FFMPEG_DIR"
Write-Ok "LIBCLANG_PATH = $env:LIBCLANG_PATH"

# ---------------------------------------------------------------------------
# 4. Build
# ---------------------------------------------------------------------------
if (-not $SkipBuild) {
    Write-Step "Building (cargo build --release)"
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        Update-SessionPath
    }
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
        throw "cargo is not on PATH. Reopen PowerShell and run: cargo build --release"
    }

    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw "Build failed (cargo build --release)." }

    Write-Ok "Build finished."
    Write-Host "`nTo run:" -ForegroundColor Cyan
    Write-Host "    .\target\release\simple-rtsp-viewer.exe cameras.json" -ForegroundColor White
}
else {
    Write-Step "Build step skipped (-SkipBuild)"
}

Write-Host "`nSetup finished." -ForegroundColor Green
