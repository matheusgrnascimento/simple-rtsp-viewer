<#
.SYNOPSIS
    Builds the Simple RTSP Viewer MSI installer.

.DESCRIPTION
    1. Builds the release executable with the C runtime linked statically, so
       the installed app does not depend on the Visual C++ Redistributable.
    2. Stages the executable, the FFmpeg DLLs and the license files.
    3. Builds dist\SimpleRtspViewer-<version>-x64.msi with the WiX Toolset.

    Requires the same environment as setup.ps1 (FFMPEG_DIR, LIBCLANG_PATH) and
    WiX Toolset 5 with the UI extension:
        dotnet tool install --global wix --version 5.0.2
        wix extension add -g WixToolset.UI.wixext/5.0.2

.PARAMETER FfmpegDir
    FFmpeg folder with bin\*.dll and LICENSE.txt. Default: $env:FFMPEG_DIR or C:\ffmpeg
#>
[CmdletBinding()]
param(
    [string]$FfmpegDir = $(if ($env:FFMPEG_DIR) { $env:FFMPEG_DIR } else { "C:\ffmpeg" })
)

$ErrorActionPreference = "Stop"
$root = Split-Path $PSScriptRoot -Parent
Set-Location $root

$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
$stage = Join-Path $root "target\installer-stage"
$dist = Join-Path $root "dist"
$msi = Join-Path $dist "SimpleRtspViewer-$version-x64.msi"

Write-Host "==> Building release $version (static C runtime)" -ForegroundColor Cyan
$env:RUSTFLAGS = "-C target-feature=+crt-static"
cargo build --release
if ($LASTEXITCODE -ne 0) { throw "cargo build failed." }

Write-Host "==> Staging files" -ForegroundColor Cyan
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path "$stage\licenses" -Force | Out-Null
Copy-Item "target\release\simple-rtsp-viewer.exe" $stage
# Only the libraries the executable loads, directly or through each other
# (avdevice, avfilter and postproc are not used).
foreach ($lib in "avcodec", "avformat", "avutil", "swresample", "swscale") {
    $dll = Get-ChildItem (Join-Path $FfmpegDir "bin") -Filter "$lib-*.dll" | Select-Object -First 1
    if (-not $dll) { throw "$lib-*.dll not found in $FfmpegDir\bin." }
    Copy-Item $dll.FullName $stage
}
Copy-Item "LICENSE" "$stage\licenses\LICENSE.txt"
Copy-Item (Join-Path $FfmpegDir "LICENSE.txt") "$stage\licenses\FFmpeg-LICENSE.txt"

# License page of the installer (RTF): the app license plus a note on FFmpeg.
$escape = { param($s) $s.Replace('\', '\\').Replace('{', '\{').Replace('}', '\}') }
$lines = (Get-Content LICENSE) + @(
    "",
    "This package includes FFmpeg (https://ffmpeg.org) shared libraries, licensed",
    "under the GNU GPL version 3. See licenses\FFmpeg-LICENSE.txt in the install",
    "folder. FFmpeg source code: https://github.com/BtbN/FFmpeg-Builds"
)
$body = ($lines | ForEach-Object { (& $escape $_) + "\par" }) -join "`n"
"{\rtf1\ansi\deff0{\fonttbl{\f0 Segoe UI;}}\fs18`n$body`n}" | Set-Content "$stage\license.rtf" -Encoding ascii

Write-Host "==> Building MSI" -ForegroundColor Cyan
New-Item -ItemType Directory -Path $dist -Force | Out-Null
wix build "installer\Package.wxs" -arch x64 -ext WixToolset.UI.wixext `
    -d "Version=$version" -d "StageDir=$stage" -o $msi
if ($LASTEXITCODE -ne 0) { throw "wix build failed." }

Write-Host "==> $msi ($([math]::Round((Get-Item $msi).Length / 1MB, 1)) MB)" -ForegroundColor Green
