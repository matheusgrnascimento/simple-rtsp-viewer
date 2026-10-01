# Simple RTSP Viewer

Simple desktop viewer for watching multiple RTSP cameras in a grid. The project is written in Rust, uses `egui`/`eframe` for the interface and FFmpeg to connect to, decode and convert the video streams.

## Features

- import of up to 64 cameras from a JSON file;
- simultaneous display of the streams in a responsive grid;
- visual indicators for connected, reconnecting and failed streams;
- automatic reconnection with progressive backoff when a stream drops;
- camera reordering by drag and drop;
- double-click to enlarge a camera (double-click or `Esc` returns to the grid);
- substream (lower resolution) in the grid, main stream only for the enlarged camera;
- frames scaled down to their on-screen size during decoding, keeping the aspect ratio;
- GPU video decoding (D3D11VA), falling back to the CPU automatically when the GPU does not support the video;
- Windows 11 look: light/dark theme following the system, Windows accent color, Segoe fonts and Mica backdrop;
- automatic persistence of the grid order in `layout.json`;
- support for RTSP URLs with authentication, without exposing credentials in the logs.

## Requirements

- stable Rust;
- FFmpeg 7.x (tested with 7.1) with shared libraries and development headers;
- libclang from LLVM 19, used by bindgen while compiling the FFmpeg bindings. `ffmpeg-sys-next` 7.1 uses bindgen 0.70, which generates invalid bindings with recent LLVM releases (LLVM 23, for example, fails).

### Windows

The `setup.ps1` script installs Rust and the Visual Studio Build Tools with `winget`, downloads FFmpeg 7.1 (shared build) and a portable copy of libclang 19 (a download of about 800 MB on the first run, no administrator rights needed), sets the environment variables and builds the executable in release mode:

```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
```

To use tools that are already installed, or to only configure FFmpeg and build:

```powershell
.\setup.ps1 -SkipInstall
```

## Manual build

With `FFMPEG_DIR`, `LIBCLANG_PATH` (the `bin` folder of an LLVM 19 install, containing `libclang.dll`) and the FFmpeg shared library directory set in the environment:

```bash
cargo build --release
```

## Camera configuration

Create a JSON file with a list of cameras. The `url` field is required; `name` and `substream_url` are optional:

```json
[
  {
    "url": "rtsp://user:password@192.168.1.10:554/stream1",
    "substream_url": "rtsp://user:password@192.168.1.10:554/stream2",
    "name": "Front door"
  },
  {
    "url": "rtsp://192.168.1.20/stream1",
    "name": "Garage"
  }
]
```

`substream_url` points to the camera's secondary stream, which has a lower resolution (on TP-Link Tapo cameras, for example, `stream1` is the main stream and `stream2` the secondary one). The grid uses it, which cuts CPU and network usage considerably; the main stream is only opened when the camera is enlarged. Without `substream_url`, the camera always uses `url`. An invalid `substream_url` is ignored without dropping the camera.

The `cameras.json` file is ignored by Git to avoid accidentally committing real addresses and credentials.

## Running

Pass the camera file as the first argument:

```bash
cargo run --release -- cameras.json
```

On Windows, after running the setup:

```powershell
.\target\release\simple-rtsp-viewer.exe cameras.json
```

Two environment variables turn off features that depend on the video driver, useful to compare performance or to work around problems:

| Variable | Effect when `0` |
|---|---|
| `RTSP_VIEWER_GPU_DECODE` | decodes on the CPU instead of the GPU |
| `RTSP_VIEWER_MICA` | uses a solid background instead of Mica |

```powershell
$env:RTSP_VIEWER_GPU_DECODE = "0"; .\target\release\simple-rtsp-viewer.exe cameras.json
```

If no path is given, the app opens the system file picker. The order set by drag and drop is saved to `layout.json` in the working directory and applied again on the next start.

## Tests

```bash
cargo test
```
