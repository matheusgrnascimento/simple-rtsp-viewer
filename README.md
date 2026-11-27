# Simple RTSP Viewer

Simple desktop viewer for watching multiple RTSP cameras in a grid. The project is written in Rust, uses `egui`/`eframe` for the interface and FFmpeg to connect to, decode and convert the video streams.

## Features

- cameras managed by the app itself: add, edit and remove them from the interface;
- local network discovery, testing each camera with the default login;
- camera logins kept in the Windows Credential Manager, never in a file;
- import of cameras from a JSON file;
- a **sidebar** (collapsible to icons) with the actions, the list of cameras with their live status, and, for the enlarged camera, its resolution, sound and a way back to the grid. Nothing floats over the video, so no control can cover another or the picture;
- simultaneous display of the streams in a responsive grid that picks the number of columns that gives the largest video for the window shape;
- visual indicators for connected, reconnecting and failed streams;
- automatic reconnection with progressive backoff when a stream drops, and a **Refresh** action to reconnect every camera;
- camera reordering by drag and drop;
- double-click a tile, or click a camera in the sidebar, to enlarge it (double-click, `Esc` or **Back to grid** returns);
- press 1 to 9 to enlarge the camera at that position; the same number again returns to the grid, another number switches to that camera;
- substream (lower resolution) everywhere by default; the enlarged camera has a **Higher resolution** switch in the sidebar to use the main stream;
- camera sound: the grid is silent, and enlarging a camera plays its audio through the default Windows output device, with mute and volume controls (see [Sound](#sound));
- frames scaled (bicubic) to their exact on-screen size in physical pixels during decoding and drawn 1:1, with borders and text aligned to the pixel grid at any display scaling, for a sharp picture and interface;
- GPU video decoding (D3D11VA), falling back to the CPU automatically when the GPU does not support the video;
- Windows 11 look: light/dark theme following the system, Windows accent color, Segoe fonts and Mica backdrop;
- grid order saved automatically;
- support for cameras with authentication, without exposing credentials in the logs.

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

## Installer

`installer\build-installer.ps1` builds two installers in `dist\`, both with the executable (C runtime linked statically, so no Visual C++ Redistributable is needed), the FFmpeg DLLs it uses and the license files. The installed executable is `Simple RTSP Viewer.exe`.

`SimpleRtspViewer-<version>-setup.exe` is a Windows 11 style installer (`installer\setup`, a small Rust program with the files compressed inside). It installs for the current user only, to `%LocalAppData%\Programs\Simple RTSP Viewer`, with no administrator rights; it updates in place, replaces an MSI install, and has its own uninstaller in Settings > Apps. It needs the Rust MSVC toolchain to build.

`SimpleRtspViewer-<version>-x64.msi` is the standard installer, for deployment tools. It installs for all users to `Program Files\Simple RTSP Viewer` by default; the Advanced button of the first page switches to the current user only (`%LocalAppData%\Programs\Simple RTSP Viewer`, no administrator prompt). It closes the running app when it updates, adds a Start menu shortcut, offers to open the app at the end and can be removed from Windows Settings > Apps. It needs [WiX Toolset 5](https://wixtoolset.org) with the UI and Util extensions:

```powershell
dotnet tool install --global wix --version 5.0.2
wix extension add -g WixToolset.UI.wixext/5.0.2
wix extension add -g WixToolset.Util.wixext/5.0.2
powershell -ExecutionPolicy Bypass -File .\installer\build-installer.ps1
```

The bundled FFmpeg build is licensed under the GPL v3; its license is installed in the `licenses` folder.

## Cameras

Cameras are managed from the sidebar (the button at its top collapses it to icons; hover an icon for its name):

- **Add camera**: address, port, main stream and substream paths, and the login (the default one, one for this camera only, or none). The "Test connection" button checks it before saving.
- **Scan network**: probes the RTSP port (554 by default) on every address of the local network, limited to 254 addresses per interface, and opens the video paths (`/stream1` and `/stream2` by default) with the default login. Cameras that respond are listed with their IP and resolution, and you choose which ones to add.
- **Import**: merges the cameras from a JSON file (format below) into the list.
- **Refresh**: reconnects every camera, without waiting for the reconnection backoff.
- **Default login**: username and password used by the cameras (and by the network scan). They are kept in the Windows Credential Manager as a generic credential of the "Simple RTSP Viewer" service.
- **Cameras list**: every camera with a status dot (green connected, amber reconnecting, red disconnected), its name and address. Click one to enlarge it (click it again to go back); right-click to edit or remove it. Right-clicking a tile in the grid offers the same.

The list is stored in `%APPDATA%\Simple RTSP Viewer\cameras.json`, without any password, already in grid order.

### Import file

The import file is a JSON file with a list of cameras. The `url` field is required; `name` and `substream_url` are optional:

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

`substream_url` points to the camera's secondary stream, which has a lower resolution (on TP-Link Tapo cameras, for example, `stream1` is the main stream and `stream2` the secondary one). The app shows it by default, also when a camera is enlarged, which cuts CPU and network usage considerably; the main stream is only opened with the **Higher resolution** button of the enlarged camera. Without `substream_url`, the camera always uses `url`. An invalid `substream_url` is ignored without dropping the camera.

On import, the login embedded in the URLs goes to the Credential Manager: the first login found becomes the default login (if there is none yet), and cameras with a different login get their own login. Cameras already in the list are skipped.

On the first run, if there is a `cameras.json` in the current folder (the format of the previous version), it is imported automatically, keeping the order from `layout.json`. After that, these two files are no longer used; since the old `cameras.json` has the password in plain text, you should delete it. It is ignored by Git to avoid accidentally committing real addresses and credentials.

## Sound

The grid never plays sound: the audio packets of every stream are read together with the video but dropped without being decoded, so the cameras in the grid cost nothing for audio. When you enlarge a camera, the app starts decoding the audio of the stream it already has open (the substream or the main one, there is no reconnection), converts it to stereo at the sample rate of the default output device and plays it. Going back to the grid, or enlarging another camera, moves or stops the sound; the output device is paused while no camera is enlarged.

The sidebar shows, in the card of the enlarged camera (only if the stream has audio):

- a **mute button**, which also stops decoding the audio while muted;
- a **volume slider**. Half way (marked with a tick) plays the camera audio at its original level and full volume boosts it by 12 dB, because camera microphones are usually quiet. The mute and volume choices are kept while the app is open, across cameras.

Any audio codec the linked FFmpeg can decode is accepted. G.711 A-law (8 kHz, mono, what TP-Link Tapo cameras send) is the one tested, with real camera data; others, such as AAC, go through the same path but were not tested. If the sound output cannot be opened (no output device, for example), the app shows an error once and the cameras stay silent.

## Running

```powershell
.\target\release\simple-rtsp-viewer.exe
```

A camera file passed as an argument is imported into the list on startup:

```powershell
.\target\release\simple-rtsp-viewer.exe other-cameras.json
```

Two environment variables turn off features that depend on the video driver, useful to compare performance or to work around problems:

| Variable | Effect when `0` |
|---|---|
| `RTSP_VIEWER_GPU_DECODE` | decodes on the CPU instead of the GPU |
| `RTSP_VIEWER_MICA` | uses a solid background instead of Mica |

```powershell
$env:RTSP_VIEWER_GPU_DECODE = "0"; .\target\release\simple-rtsp-viewer.exe
```

## Tests

```bash
cargo test
```

The interface has layout tests that draw it without a window at several window sizes (down to the 720x480 minimum), display scales (100%, 150%, 200%), themes and numbers of cameras (0 to 64), and fail if any component overlaps another, leaves its area or covers the sidebar. They do not open any camera connection. Two manual benchmarks exist for tuning (they are ignored by default):

```bash
# cost of each pixel-converter filter (swscale), per frame
cargo test --release scaler_benchmark -- --ignored --nocapture
# cost of one frame of the interface
cargo test --release ui_frame_benchmark -- --ignored --nocapture
```

## License

MIT. See [LICENSE](LICENSE).
