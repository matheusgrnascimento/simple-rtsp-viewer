<#
.SYNOPSIS
    Sets up the Windows environment and builds Simple RTSP Viewer.

.DESCRIPTION
    Runs, end to end and from PowerShell:
      1. Installs the build tools (Rust, Visual Studio Build Tools) with winget.
      2. Downloads and extracts the FFmpeg 7.1 libraries ("shared" build with headers/libs/DLLs).
      3. Downloads a portable copy of libclang from LLVM 19.1.7, used by bindgen
         (no administrator rights needed).
      4. Sets the required environment variables (FFMPEG_DIR, LIBCLANG_PATH, PATH),
         permanently (user level) and also in the current session.
      5. Builds the project in release mode.

    The FFmpeg and LLVM versions are pinned because ffmpeg-next 7.1 (and the
    bindgen 0.70 it uses) do not keep up with the newest releases.

    Run it from the project folder:
        powershell -ExecutionPolicy Bypass -File .\setup.ps1

.PARAMETER FfmpegDir
    FFmpeg destination folder. Default: C:\ffmpeg

.PARAMETER FfmpegUrl
    FFmpeg zip to download. Default: the 7.1 "gpl-shared" build from BtbN/FFmpeg-Builds.
    If the link stops working, pick another "n7.1 ... win64-gpl-shared-7.1.zip" file
    at https://github.com/BtbN/FFmpeg-Builds/releases and pass it here.

.PARAMETER LibclangDir
    "bin" folder of an LLVM 19 install containing libclang.dll. If libclang.dll does not
    exist there, LLVM 19.1.7 is downloaded and extracted into the parent folder.
    Default: %LOCALAPPDATA%\Programs\LLVM-19.1.7\bin

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
    # BtbN's "latest" channel no longer ships FFmpeg 7.1; this is the last
    # monthly build of the 7.1 series published there (2026-07-31).
    [string]$FfmpegUrl = "https://github.com/BtbN/FFmpeg-Builds/releases/download/autobuild-2026-07-31-14-10/ffmpeg-n7.1.5-12-g1fdbca85aa-win64-gpl-shared-7.1.zip",
    [string]$LibclangDir = (Join-Path $env:LOCALAPPDATA "Programs\LLVM-19.1.7\bin"),
    [switch]$SkipInstall,
    [switch]$SkipBuild,
    [switch]$Force
)

$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot

# LLVM version downloaded when there is no libclang.dll in $LibclangDir.
$llvmVersion = "19.1.7"

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
    # LLVM is not installed with winget: the "LLVM.LLVM" package always ships
    # the newest version, which is incompatible with bindgen 0.70 (see step 3).
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
    Write-Step "Downloading and extracting FFmpeg 7.1 (shared build)"
    $zip   = Join-Path $env:TEMP "ffmpeg-shared.zip"
    $tmp   = Join-Path $env:TEMP "ffmpeg-extract"

    Write-Host "    Downloading: $FfmpegUrl"
    # Without the progress bar, Invoke-WebRequest in Windows PowerShell 5.1
    # downloads large files much faster.
    $ProgressPreference = "SilentlyContinue"
    Invoke-WebRequest -Uri $FfmpegUrl -OutFile $zip

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

# The project was tested with FFmpeg 7.1 (libavcodec 61). An old folder with
# FFmpeg 8+ (for example, from an earlier version of this script, which
# downloaded the "master" build) will probably not build with ffmpeg-next 7.1.
$versionHeader = Join-Path $ffInc "libavcodec\version_major.h"
$avcodecMajor = $null
if (Test-Path $versionHeader) {
    $match = Select-String -Path $versionHeader -Pattern 'define\s+LIBAVCODEC_VERSION_MAJOR\s+(\d+)' |
        Select-Object -First 1
    if ($match) { $avcodecMajor = [int]$match.Matches[0].Groups[1].Value }
}
if ($null -eq $avcodecMajor) {
    Write-Warn2 "Could not detect the FFmpeg version in $FfmpegDir."
}
elseif ($avcodecMajor -gt 61) {
    Write-Warn2 "The FFmpeg in $FfmpegDir is 8.x or newer (libavcodec $avcodecMajor); the project was tested with 7.1."
    Write-Warn2 "If the build fails, run again with -Force to download FFmpeg 7.1."
}
else {
    Write-Ok "FFmpeg in $FfmpegDir (libavcodec $avcodecMajor)."
}

# ---------------------------------------------------------------------------
# 3. libclang (used by the ffmpeg-sys-next bindgen)
# ---------------------------------------------------------------------------
# ffmpeg-sys-next 7.1 uses bindgen 0.70, which does not work with a recent
# libclang: with LLVM 23, for example, it generates empty structs
# (`_address: u8`) and the build fails. That is why the setup uses a portable
# copy of libclang 19, extracted from the official LLVM package, instead of a
# system-wide LLVM install.
$libclangDll = Join-Path $LibclangDir "libclang.dll"

if (Test-Path $libclangDll) {
    Write-Step "libclang already present in $LibclangDir"
}
else {
    Write-Step "Downloading libclang from LLVM $llvmVersion (~800 MB package, first run only)"

    # Uses the tar that ships with Windows: it extracts .tar.xz and understands "C:\" paths.
    $tarExe = Join-Path $env:SystemRoot "System32\tar.exe"
    if (-not (Test-Path $tarExe)) {
        throw "tar.exe not found at $tarExe (included in Windows 10 1803 or newer)."
    }

    $llvmName = "clang+llvm-$llvmVersion-x86_64-pc-windows-msvc"
    $llvmUrl  = "https://github.com/llvm/llvm-project/releases/download/llvmorg-$llvmVersion/$llvmName.tar.xz"
    $llvmTmp  = Join-Path $env:TEMP "llvm-$llvmVersion-download"
    $archive  = Join-Path $llvmTmp "$llvmName.tar.xz"

    if (Test-Path $llvmTmp) { Remove-Item $llvmTmp -Recurse -Force }
    New-Item -ItemType Directory -Path $llvmTmp | Out-Null

    Write-Host "    Downloading: $llvmUrl"
    curl.exe -fsSL -o $archive $llvmUrl
    if ($LASTEXITCODE -ne 0) { throw "Failed to download LLVM (curl returned $LASTEXITCODE)." }

    # Only libclang.dll and clang's internal headers (lib\clang) are needed
    # by bindgen; the rest of the package is discarded.
    Write-Host "    Extracting libclang.dll and the clang headers..."
    & $tarExe -xf $archive -C $llvmTmp "$llvmName/bin/libclang.dll" "$llvmName/lib/clang"
    if ($LASTEXITCODE -ne 0) { throw "Failed to extract LLVM (tar returned $LASTEXITCODE)." }

    $llvmRoot = Split-Path $LibclangDir -Parent
    New-Item -ItemType Directory -Path $llvmRoot -Force | Out-Null
    Copy-Item -Path (Join-Path $llvmTmp "$llvmName\*") -Destination $llvmRoot -Recurse -Force

    Remove-Item $llvmTmp -Recurse -Force -ErrorAction SilentlyContinue

    if (-not (Test-Path $libclangDll)) { throw "libclang.dll not found in $LibclangDir after extraction." }
    Write-Ok "libclang $llvmVersion installed in $LibclangDir."
}

# ---------------------------------------------------------------------------
# 4. Environment variables
# ---------------------------------------------------------------------------
Write-Step "Setting environment variables"

# Permanent (user level).
[Environment]::SetEnvironmentVariable("FFMPEG_DIR", $FfmpegDir, "User")
[Environment]::SetEnvironmentVariable("LIBCLANG_PATH", $LibclangDir, "User")

$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
if ($userPath -notlike "*$ffBin*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$ffBin", "User")
    Write-Ok "Added $ffBin to the user PATH."
}

# Current session (so the build right below works without reopening the terminal).
$env:FFMPEG_DIR    = $FfmpegDir
$env:LIBCLANG_PATH = $LibclangDir
if ($env:Path -notlike "*$ffBin*") { $env:Path += ";$ffBin" }

Write-Ok "FFMPEG_DIR    = $env:FFMPEG_DIR"
Write-Ok "LIBCLANG_PATH = $env:LIBCLANG_PATH"

# ---------------------------------------------------------------------------
# 5. Build
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

    # Copies the FFmpeg DLLs next to the executable, so it starts even from
    # terminals or shortcuts whose PATH does not include $ffBin yet (for
    # example, those opened before this setup). Without them, Windows closes
    # the program silently, before any message.
    $releaseDir = Join-Path $PSScriptRoot "target\release"
    Copy-Item -Path (Join-Path $ffBin "*.dll") -Destination $releaseDir -Force
    Write-Ok "FFmpeg DLLs copied to $releaseDir."
    Write-Host "`nTo run:" -ForegroundColor Cyan
    Write-Host "    .\target\release\simple-rtsp-viewer.exe" -ForegroundColor White
}
else {
    Write-Step "Build step skipped (-SkipBuild)"
}

Write-Host "`nSetup finished." -ForegroundColor Green
