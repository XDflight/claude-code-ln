//! Best-effort lookup of the e-mail / organization name behind the uuids.
//!
//! The desktop app names its directories by uuid only. The claude.ai page it hosts,
//! however, persists its query cache in IndexedDB, and that cache contains the signed-in
//! account (`uuid`, `email_address`, …) and the organizations it belongs to (`uuid`,
//! `name`). The cache is a V8-serialized value, usually Snappy-compressed; instead of a
//! full deserializer this scans for `uuid` keys directly followed by the keys of interest.
//! Only the account currently signed in is in there, so callers should remember results.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::store::is_uuid;

const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_DECOMPRESSED_BYTES: usize = 256 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 400;

/// Reads one serialized string (one-byte `"` or UTF-16 `c`) at `pos`.
fn read_string(buf: &[u8], mut pos: usize) -> Option<(String, usize)> {
    // Two-byte strings are preceded by alignment padding.
    while *buf.get(pos)? == 0 {
        pos += 1;
    }
    let tag = buf[pos];
    pos += 1;
    let mut len = 0usize;
    let mut shift = 0;
    loop {
        let byte = *buf.get(pos)?;
        pos += 1;
        len |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 14 {
            return None;
        }
    }
    if len > MAX_TEXT_BYTES {
        return None;
    }
    let bytes = buf.get(pos..pos + len)?;
    let text = match tag {
        b'"' => bytes.iter().map(|&b| char::from(b)).collect(),
        b'c' if len.is_multiple_of(2) => {
            let units: Vec<u16> =
                bytes.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            String::from_utf16(&units).ok()?
        }
        _ => return None,
    };
    Some((text, pos + len))
}

fn plausible(text: &str) -> bool {
    !text.trim().is_empty() && !text.chars().any(char::is_control)
}

/// Collects `uuid -> label` pairs from one (decompressed) serialized value.
fn scan(buf: &[u8], out: &mut BTreeMap<String, String>) {
    const KEY: &[u8] = b"\"\x04uuid";
    let mut from = 0;
    while let Some(found) = buf[from..].windows(KEY.len()).position(|w| w == KEY) {
        let after_key = from + found + KEY.len();
        from = after_key;
        let Some((uuid, mut pos)) = read_string(buf, after_key) else { continue };
        if !is_uuid(&uuid) {
            continue;
        }
        // An organization is `uuid, name, …`; an account is `uuid, email_address, …`.
        for _ in 0..4 {
            let Some((key, value_pos)) = read_string(buf, pos) else { break };
            let Some((value, next)) = read_string(buf, value_pos) else { break };
            pos = next;
            match key.as_str() {
                "name" | "email_address" if plausible(&value) => {
                    if key == "name" || value.contains('@') {
                        out.insert(uuid.clone(), value.trim().to_owned());
                    }
                    break;
                }
                "full_name" | "display_name" | "tagged_id" => {}
                _ => break,
            }
        }
    }
}

/// Chromium wraps large IndexedDB values as `ff <version> 02 <snappy data>`.
fn decode(bytes: Vec<u8>) -> Vec<u8> {
    if bytes.len() > 3 && bytes[0] == 0xff && bytes[2] == 0x02 {
        let body = &bytes[3..];
        if snap::raw::decompress_len(body).is_ok_and(|n| n <= MAX_DECOMPRESSED_BYTES)
            && let Ok(plain) = snap::raw::Decoder::new().decompress_vec(body)
        {
            return plain;
        }
    }
    bytes
}

fn collect_files(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.filter_map(Result::ok) {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else { continue };
        if meta.is_dir() && depth > 0 {
            collect_files(&entry.path(), depth - 1, out);
        } else if meta.is_file() && meta.len() <= MAX_FILE_BYTES {
            out.push(entry.path());
        }
    }
}

/// Scans the desktop app's data directory (the parent of `claude-code-sessions`).
/// Read-only; returns `uuid -> e-mail` for accounts and `uuid -> name` for organizations.
pub fn harvest(app_data_dir: &Path) -> BTreeMap<String, String> {
    let mut files = Vec::new();
    let Ok(stores) = fs::read_dir(app_data_dir.join("IndexedDB")) else {
        return BTreeMap::new();
    };
    for store in stores.filter_map(Result::ok) {
        if store.file_name().to_string_lossy().contains("claude.ai") {
            collect_files(&store.path(), 4, &mut files);
        }
    }
    // Oldest first, so the most recently written value wins.
    files.sort_by_key(|f| fs::metadata(f).and_then(|m| m.modified()).ok());
    let mut out = BTreeMap::new();
    for file in files {
        if let Ok(bytes) = fs::read(&file) {
            scan(&decode(bytes), &mut out);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCOUNT: &str = "aaaaaaaa-0000-4000-8000-000000000001";
    const ORG: &str = "bbbbbbbb-0000-4000-8000-000000000001";
    const ORG2: &str = "bbbbbbbb-0000-4000-8000-000000000002";

    fn one_byte(text: &str) -> Vec<u8> {
        let mut v = vec![b'"', text.len() as u8];
        v.extend_from_slice(text.as_bytes());
        v
    }

    fn two_byte(text: &str) -> Vec<u8> {
        let units: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut v = vec![0, b'c', units.len() as u8];
        v.extend(units);
        v
    }

    fn sample() -> Vec<u8> {
        let mut v = b"\xff\x15o".to_vec();
        for part in [
            one_byte("uuid"), one_byte(ACCOUNT), one_byte("email_address"), one_byte("a@b.co"),
            one_byte("full_name"), one_byte("X"), one_byte("organization"), vec![b'o'],
            one_byte("uuid"), one_byte(ORG), one_byte("name"), one_byte("Acme"),
            one_byte("uuid"), one_byte(ORG2), one_byte("name"), two_byte("组织"),
            // A uuid followed by something else is ignored.
            one_byte("uuid"), one_byte("cccccccc-0000-4000-8000-000000000001"), one_byte("kind"),
            one_byte("x"),
        ] {
            v.extend(part);
        }
        v
    }

    #[test]
    fn finds_accounts_and_organizations() {
        let mut out = BTreeMap::new();
        scan(&sample(), &mut out);
        assert_eq!(out.len(), 3);
        assert_eq!(out[ACCOUNT], "a@b.co");
        assert_eq!(out[ORG], "Acme");
        assert_eq!(out[ORG2], "组织");
    }

    #[test]
    fn reads_snappy_wrapped_blobs_from_disk() {
        let base = std::env::temp_dir().join(format!("ccln-names-{}", std::process::id()));
        let dir = base.join("IndexedDB").join("https_claude.ai_0.indexeddb.blob").join("5").join("00");
        fs::create_dir_all(&dir).unwrap();
        let mut blob = vec![0xff, 0x11, 0x02];
        blob.extend(snap::raw::Encoder::new().compress_vec(&sample()).unwrap());
        fs::write(dir.join("21"), blob).unwrap();
        let out = harvest(&base);
        assert_eq!(out.get(ORG).map(String::as_str), Some("Acme"));
        let _ = fs::remove_dir_all(&base);
        // Truncated / garbage input must not panic.
        let mut out = BTreeMap::new();
        scan(b"\"\x04uuid\"\x24short", &mut out);
        scan(&sample()[..40], &mut out);
    }
}
