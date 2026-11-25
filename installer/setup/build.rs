//! Embeds the resources of the setup and the files it installs.
//!
//! The files come from the folder in `SETUP_PAYLOAD_DIR` (staged by
//! `installer/build-installer.ps1`). Without it the setup still builds (for
//! `cargo check` and clippy) and refuses to install.
//!
//! The version of the app is read from its `Cargo.toml`.

#[path = "resources.rs"]
mod resources;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use windows::Win32::Storage::Compression::{
    CloseCompressor, Compress, CreateCompressor, COMPRESSOR_HANDLE, COMPRESS_ALGORITHM_LZMS,
};

/// The size of the chunks the table is compressed in.
const CHUNK: usize = 64 << 20;

fn main() {
    let here = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("set by cargo"));
    let root = here.join("..").join("..");
    let version = app_version(&root.join("Cargo.toml"));
    println!(
        "cargo:rerun-if-changed={}",
        root.join("Cargo.toml").display()
    );
    println!("cargo:rustc-env=APP_VERSION={version}");
    println!("cargo:rerun-if-env-changed=SETUP_PAYLOAD_DIR");
    let mut blob = Vec::new();
    if let Some(dir) = env::var_os("SETUP_PAYLOAD_DIR").map(PathBuf::from) {
        let mut files = Vec::new();
        collect(&dir, &dir, &mut files);
        files.sort();
        blob = pack(&files);
    }
    resources::embed(
        &here.join("app.manifest"),
        &root.join("assets").join("icon.ico"),
        &resources::Info {
            description: "Simple RTSP Viewer Setup",
            file_name: "Simple RTSP Viewer Setup",
            product: "Simple RTSP Viewer",
            version: &version,
            payload: &blob,
        },
    );
    println!(
        "cargo:rerun-if-changed={}",
        root.join("assets").join("icon.ico").display()
    );
    println!("cargo:rerun-if-changed=app.manifest");
}

/// The `version = "..."` of the app's `[package]`.
fn app_version(cargo_toml: &Path) -> String {
    let text = fs::read_to_string(cargo_toml).expect("read the app's Cargo.toml");
    text.lines()
        .find_map(|line| line.strip_prefix("version = \""))
        .and_then(|rest| rest.split('"').next())
        .expect("version in Cargo.toml")
        .to_string()
}

/// Lists the files under `dir` (paths relative to `base`, with `/`).
fn collect(base: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) {
    for entry in fs::read_dir(dir).expect("read the payload folder") {
        let path = entry.expect("read the payload folder").path();
        if path.is_dir() {
            collect(base, &path, files);
        } else {
            println!("cargo:rerun-if-changed={}", path.display());
            let relative = path.strip_prefix(base).expect("under the base");
            let name = relative
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            files.push((name, path));
        }
    }
}

/// Builds the blob described in `src/payload.rs`.
fn pack(files: &[(String, PathBuf)]) -> Vec<u8> {
    let mut table = Vec::new();
    table.extend_from_slice(&(files.len() as u32).to_le_bytes());
    for (name, path) in files {
        let data = fs::read(path).expect("read a payload file");
        table.extend_from_slice(&(name.len() as u16).to_le_bytes());
        table.extend_from_slice(name.as_bytes());
        table.extend_from_slice(&(data.len() as u64).to_le_bytes());
        table.extend_from_slice(&data);
    }
    let mut blob = b"SPK1".to_vec();
    blob.extend_from_slice(&(table.len() as u64).to_le_bytes());
    let mut handle = COMPRESSOR_HANDLE::default();
    // SAFETY: the handle is closed below; each call reads one chunk of
    // `table` and writes into a buffer of the size it is given.
    unsafe {
        CreateCompressor(COMPRESS_ALGORITHM_LZMS, None, &mut handle).expect("create compressor");
        for chunk in table.chunks(CHUNK) {
            let mut packed = vec![0u8; chunk.len() + chunk.len() / 8 + 4096];
            let mut size = 0;
            Compress(
                handle,
                Some(chunk.as_ptr().cast()),
                chunk.len(),
                Some(packed.as_mut_ptr().cast()),
                packed.len(),
                &mut size,
            )
            .expect("compress the payload");
            blob.extend_from_slice(&(chunk.len() as u32).to_le_bytes());
            blob.extend_from_slice(&(size as u32).to_le_bytes());
            blob.extend_from_slice(&packed[..size]);
        }
        let _ = CloseCompressor(handle);
    }
    blob
}
