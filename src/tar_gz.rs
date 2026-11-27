//! Minimal libz gzip decompressor and POSIX/GNU/PAX tar extractor.

use crate::sha256::{Sha256, hex_lower};
use std::{
    collections::BTreeMap,
    ffi::{c_char, c_int, c_uint, c_ulong, c_void},
    fs,
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    ptr,
};

#[repr(C)]
struct ZStream {
    next_in: *const u8,
    avail_in: c_uint,
    total_in: c_ulong,
    next_out: *mut u8,
    avail_out: c_uint,
    total_out: c_ulong,
    msg: *const c_char,
    state: *mut c_void,
    zalloc: *mut c_void,
    zfree: *mut c_void,
    opaque: *mut c_void,
    data_type: c_int,
    adler: c_ulong,
    reserved: c_ulong,
}

const Z_NO_FLUSH: c_int = 0;
const Z_OK: c_int = 0;
const Z_STREAM_END: c_int = 1;
const Z_BUF_ERROR: c_int = -5;

#[link(name = "z")]
unsafe extern "C" {
    fn zlibVersion() -> *const c_char;
    fn inflateInit2_(
        strm: *mut ZStream,
        window_bits: c_int,
        version: *const c_char,
        stream_size: c_int,
    ) -> c_int;
    fn inflate(strm: *mut ZStream, flush: c_int) -> c_int;
    fn inflateReset(strm: *mut ZStream) -> c_int;
    fn inflateEnd(strm: *mut ZStream) -> c_int;
}

pub fn gunzip(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut strm = Box::new(ZStream {
        next_in: ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: ptr::null(),
        state: ptr::null_mut(),
        zalloc: ptr::null_mut(),
        zfree: ptr::null_mut(),
        opaque: ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    });
    let rc = unsafe {
        inflateInit2_(
            &mut *strm,
            15 + 16,
            zlibVersion(),
            std::mem::size_of::<ZStream>() as c_int,
        )
    };
    if rc != Z_OK {
        return Err(format!("zlib inflateInit2 failed: {rc}"));
    }
    struct Guard(Box<ZStream>);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                inflateEnd(&mut *self.0);
            }
        }
    }
    let mut guard = Guard(strm);
    let hint = if input.len() >= 18 {
        let isize = u32::from_le_bytes(input[input.len() - 4..].try_into().unwrap()) as usize;
        isize.clamp(4096, 64 * 1024 * 1024)
    } else {
        4096
    };
    let mut out = Vec::with_capacity(hint);
    let mut chunk_buf = [0u8; 65536];
    let mut pos = 0usize;
    loop {
        let rem = &input[pos..];
        let avail_in = rem.len().min(u32::MAX as usize);
        guard.0.next_in = rem.as_ptr();
        guard.0.avail_in = avail_in as c_uint;
        guard.0.next_out = chunk_buf.as_mut_ptr();
        guard.0.avail_out = chunk_buf.len() as c_uint;
        let rc = unsafe { inflate(&mut *guard.0, Z_NO_FLUSH) };
        let consumed = avail_in - guard.0.avail_in as usize;
        let produced = chunk_buf.len() - guard.0.avail_out as usize;
        pos += consumed;
        if produced > 0 {
            out.extend_from_slice(&chunk_buf[..produced]);
        }
        match rc {
            Z_OK => {
                if consumed == 0 && produced == 0 {
                    return Err("truncated gzip stream".into());
                }
            }
            Z_STREAM_END => {
                if pos < input.len() && input[pos..].starts_with(&[0x1f, 0x8b]) {
                    let r = unsafe { inflateReset(&mut *guard.0) };
                    if r != Z_OK {
                        return Err(format!("zlib inflateReset failed: {r}"));
                    }
                } else {
                    return Ok(out);
                }
            }
            Z_BUF_ERROR if produced > 0 => {}
            _ => return Err(format!("zlib inflate failed: {rc}")),
        }
    }
}

fn parse_octal(field: &[u8]) -> Result<u64, String> {
    if field.first() == Some(&0x80) {
        // GNU base-256 extension
        let mut val = 0u64;
        for &b in &field[1..] {
            val = (val << 8) | (b as u64);
        }
        return Ok(val);
    }
    let s = std::str::from_utf8(field)
        .map_err(|_| "non-utf8 tar header number")?
        .trim_matches(|c: char| c == '\0' || c == ' ');
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|e| format!("invalid tar octal {s:?}: {e}"))
}

fn cstr_trim(field: &[u8]) -> String {
    let end = field.iter().position(|&b| b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

fn parse_pax_path(data: &[u8]) -> Option<String> {
    let mut pos = 0;
    while pos < data.len() {
        let sp = data[pos..].iter().position(|&b| b == b' ')?;
        let len: usize = std::str::from_utf8(&data[pos..pos + sp]).ok()?.parse().ok()?;
        if len == 0 || pos + len > data.len() {
            break;
        }
        let rec = &data[pos + sp + 1..pos + len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        if let Some(val) = rec.strip_prefix(b"path=") {
            return Some(String::from_utf8_lossy(val).into_owned());
        }
        pos += len;
    }
    None
}

fn safe_strip_first(raw: &str) -> Result<Option<PathBuf>, String> {
    let mut comps = Path::new(raw).components();
    let _first = comps.next(); // strip top-level "<crate>-<version>/"
    let mut out = PathBuf::new();
    for c in comps {
        match c {
            Component::Normal(seg) => out.push(seg),
            Component::CurDir => {}
            _ => return Err(format!("unsafe path in crate archive: {raw}")),
        }
    }
    if out.as_os_str().is_empty() {
        Ok(None)
    } else {
        Ok(Some(out))
    }
}

/// Extract a `.crate` (`.tar.gz`) archive into `dest`, stripping the leading
/// `<name>-<version>/` component. If `compute_hashes` is true, returns a map of
/// relative file paths to SHA-256 hex digests (for `.cargo-checksum.json`).
pub fn extract_crate(
    archive: &Path,
    dest: &Path,
    compute_hashes: bool,
) -> Result<BTreeMap<String, String>, String> {
    let raw = fs::read(archive).map_err(|e| format!("reading {}: {e}", archive.display()))?;
    let tar = if raw.starts_with(&[0x1f, 0x8b]) {
        gunzip(&raw)?
    } else {
        raw
    };
    fs::create_dir_all(dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;

    let mut hashes = BTreeMap::new();
    let mut pos = 0usize;
    let mut next_long_name: Option<String> = None;

    while pos + 512 <= tar.len() {
        let hdr = &tar[pos..pos + 512];
        pos += 512;
        if hdr.iter().all(|&b| b == 0) {
            break;
        }
        let size = parse_octal(&hdr[124..136])? as usize;
        let mode = parse_octal(&hdr[100..108]).unwrap_or(0o644) as u32;
        let typeflag = hdr[156];
        let data_end = pos
            .checked_add(size)
            .ok_or_else(|| "tar entry size overflow".to_owned())?;
        if data_end > tar.len() {
            return Err("truncated tar archive".into());
        }
        let data = &tar[pos..data_end];
        let padded = size.div_ceil(512) * 512;
        pos = (pos + padded).min(tar.len());

        match typeflag {
            b'L' => {
                // GNU long name
                let s = cstr_trim(data);
                next_long_name = Some(s);
                continue;
            }
            b'x' => {
                // PAX extended header for next entry
                if let Some(p) = parse_pax_path(data) {
                    next_long_name = Some(p);
                }
                continue;
            }
            b'g' => continue, // PAX global header
            _ => {}
        }

        let name = if let Some(n) = next_long_name.take() {
            n
        } else {
            let base = cstr_trim(&hdr[0..100]);
            let prefix = cstr_trim(&hdr[345..500]);
            if prefix.is_empty() {
                base
            } else {
                format!("{prefix}/{base}")
            }
        };

        let Some(rel) = safe_strip_first(&name)? else {
            continue;
        };
        let target = dest.join(&rel);
        match typeflag {
            b'5' => {
                fs::create_dir_all(&target)
                    .map_err(|e| format!("mkdir {}: {e}", target.display()))?;
            }
            b'0' | 0 => {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
                }
                fs::write(&target, data).map_err(|e| format!("write {}: {e}", target.display()))?;
                if mode & 0o111 != 0 {
                    let _ = fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777));
                }
                if compute_hashes {
                    let rel_str = rel
                        .to_str()
                        .ok_or_else(|| format!("non-utf8 path: {}", rel.display()))?
                        .to_owned();
                    hashes.insert(rel_str, hex_lower(&Sha256::digest(data)));
                }
            }
            b'2' => {
                // Symlink inside crate (rare)
                let link_target = cstr_trim(&hdr[157..257]);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
                }
                let _ = std::os::unix::fs::symlink(link_target, &target);
            }
            _ => {}
        }
    }
    Ok(hashes)
}
