//! The files this setup installs: a table of files, compressed in chunks
//! with the Windows LZMS compressor by `build.rs`, and embedded as the
//! `RT_RCDATA` resource `PAYLOAD_ID`.
//!
//! Layout of the embedded blob:
//! - `SPK1`, then the size of the table in bytes (u64);
//! - the chunks of the compressed table: raw size (u32), compressed size
//!   (u32) and the data.
//!
//! The table: the number of files (u32), then for each its relative path
//! (u16 length, UTF-8) and its contents (u64 length, bytes).

use std::path::Path;

use windows::core::{Error, Result, HRESULT, PCWSTR};
use windows::Win32::Storage::Compression::{
    CloseDecompressor, CreateDecompressor, Decompress, COMPRESS_ALGORITHM_LZMS, DECOMPRESSOR_HANDLE,
};
use windows::Win32::System::LibraryLoader::{
    BeginUpdateResourceW, EndUpdateResourceW, FindResourceW, LoadResource, LockResource,
    SizeofResource, UpdateResourceW,
};

const MAGIC: &[u8; 4] = b"SPK1";
/// `RT_RCDATA`, and the id `build.rs` gives the payload.
const RT_RCDATA: PCWSTR = PCWSTR(10 as _);
const PAYLOAD_ID: PCWSTR = PCWSTR(2 as _);

/// The embedded blob (empty if the setup was built without files).
pub fn embedded() -> &'static [u8] {
    // SAFETY: a resource of this module stays mapped for the life of the
    // process; the size comes from the same resource.
    unsafe {
        let found = FindResourceW(None, PAYLOAD_ID, RT_RCDATA);
        if found.is_invalid() {
            return &[];
        }
        let Ok(loaded) = LoadResource(None, found) else {
            return &[];
        };
        let data = LockResource(loaded);
        let size = SizeofResource(None, found) as usize;
        if data.is_null() || size == 0 {
            return &[];
        }
        std::slice::from_raw_parts(data.cast(), size)
    }
}

/// Removes the embedded files from the executable at `path`: the
/// uninstaller does not need them. Does nothing if it fails.
pub fn strip(path: &Path) {
    let wide: Vec<u16> = path.to_string_lossy().encode_utf16().chain([0]).collect();
    // SAFETY: a null-terminated path; the update handle is closed by
    // `EndUpdateResourceW`, discarding the change if it did not work.
    unsafe {
        let Ok(handle) = BeginUpdateResourceW(PCWSTR(wide.as_ptr()), false) else {
            return;
        };
        let removed = UpdateResourceW(handle, RT_RCDATA, PAYLOAD_ID, 0, None, 0).is_ok();
        let _ = EndUpdateResourceW(handle, !removed);
    }
}

fn corrupt() -> Error {
    Error::new(HRESULT(-1), "The setup is damaged. Download it again.")
}

/// Takes `n` bytes from the front of `data`.
fn take<'a>(data: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
    if data.len() < n {
        return Err(corrupt());
    }
    let (head, tail) = data.split_at(n);
    *data = tail;
    Ok(head)
}

fn u32_of(bytes: &[u8]) -> usize {
    u32::from_le_bytes(bytes.try_into().expect("4 bytes")) as usize
}

fn u64_of(bytes: &[u8]) -> usize {
    u64::from_le_bytes(bytes.try_into().expect("8 bytes")) as usize
}

/// Decompresses the blob into the table of files.
pub fn unpack(blob: &[u8]) -> Result<Vec<u8>> {
    let mut rest = blob;
    if take(&mut rest, 4)? != MAGIC {
        return Err(corrupt());
    }
    let total = u64_of(take(&mut rest, 8)?);
    let mut table = vec![0u8; total];
    let mut handle = DECOMPRESSOR_HANDLE::default();
    // SAFETY: the handle is closed below; every chunk is decompressed into a
    // slice of `table` of the size the chunk declares.
    unsafe {
        CreateDecompressor(COMPRESS_ALGORITHM_LZMS, None, &mut handle)?;
        let mut done = 0;
        let result = (|| -> Result<()> {
            while done < total {
                let raw = u32_of(take(&mut rest, 4)?);
                let packed = u32_of(take(&mut rest, 4)?);
                let chunk = take(&mut rest, packed)?;
                if raw > total - done {
                    return Err(corrupt());
                }
                let mut written = 0;
                Decompress(
                    handle,
                    Some(chunk.as_ptr().cast()),
                    chunk.len(),
                    Some(table[done..].as_mut_ptr().cast()),
                    raw,
                    Some(&mut written),
                )?;
                if written != raw {
                    return Err(corrupt());
                }
                done += raw;
            }
            Ok(())
        })();
        let _ = CloseDecompressor(handle);
        result?;
    }
    Ok(table)
}

/// The files of the table: relative path (with `/`) and contents.
pub fn files(table: &[u8]) -> Result<Vec<(&str, &[u8])>> {
    let mut rest = table;
    let count = u32_of(take(&mut rest, 4)?);
    let mut files = Vec::new();
    for _ in 0..count {
        let name_len = usize::from(u16::from_le_bytes(
            take(&mut rest, 2)?.try_into().expect("2 bytes"),
        ));
        let name = std::str::from_utf8(take(&mut rest, name_len)?).map_err(|_| corrupt())?;
        // A path from this table is joined to the install folder: it must
        // stay inside it.
        if name.is_empty()
            || name.starts_with('/')
            || name.contains(':')
            || name.contains('\\')
            || name.split('/').any(|part| part.is_empty() || part == "..")
        {
            return Err(corrupt());
        }
        let len = u64_of(take(&mut rest, 8)?);
        files.push((name, take(&mut rest, len)?));
    }
    Ok(files)
}
