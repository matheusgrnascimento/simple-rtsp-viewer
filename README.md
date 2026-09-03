# Simple RTSP Viewer

Simple desktop viewer for watching multiple RTSP cameras in a grid. The project is written in Rust, uses `egui`/`eframe` for the interface and FFmpeg to connect to, decode and convert the video streams.

## Features

- import of up to 64 cameras from a JSON file;
- simultaneous display of the streams in a responsive grid;
- visual indicators for connected, reconnecting and failed streams;
- automatic reconnection with progressive backoff when a stream drops;
- camera reordering by drag and drop;
- automatic persistence of the grid order in `layout.json`;
- support for RTSP URLs with authentication, without exposing credentials in the logs.

## Requirements

- stable Rust;
- FFmpeg 7 with shared libraries and development headers;
- LLVM/Clang, needed while compiling the FFmpeg bindings.

### Windows

The `setup.ps1` script installs the required tools with `winget`, downloads a shared FFmpeg build, sets the environment variables and builds the executable in release mode:

```powershell
powershell -ExecutionPolicy Bypass -File .\setup.ps1
```

To use tools that are already installed, or to only configure FFmpeg and build:

```powershell
.\setup.ps1 -SkipInstall
```

## Manual build

With `FFMPEG_DIR`, `LIBCLANG_PATH` and the FFmpeg shared library directory set in the environment:

```bash
cargo build --release
```

## Camera configuration

Create a JSON file with a list of cameras. The `url` field is required and `name` is optional:

```json
[
  {
    "url": "rtsp://user:password@192.168.1.10:554/stream1",
    "name": "Front door"
  },
  {
    "url": "rtsp://192.168.1.20/stream1",
    "name": "Garage"
  }
]
```

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

If no path is given, the app opens the system file picker. The order set by drag and drop is saved to `layout.json` in the working directory and applied again on the next start.

## Tests

```bash
cargo test
```
