//! Strict codeload `tar.gz` unpacking and source-tree inventory.
//!
//! The tap archive is data only: every member is unpacked under strict
//! path, type, count, and byte limits so a hostile archive cannot escape
//! the staging tree or bomb the host. See [`extract_tar_gz`].

use super::safe_segment;
use super::sha256_hex;
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Read};
use std::path::Path;

/// Codeload download byte limit (compressed archive).
pub(super) const MAX_ARCHIVE_BYTES: usize = 96 * 1024 * 1024;
/// Total unpacked tree limit.
const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum number of files in the tap tree.
const MAX_FILES: usize = 20_000;
/// Per-file unpacked length limit.
const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum size accepted for one PAX global metadata header.
const MAX_PAX_HEADER_BYTES: u64 = 64 * 1024;

pub(super) fn tree_inventory(tree: &Path) -> Result<BTreeMap<String, String>, String> {
    let mut inventory = BTreeMap::new();
    let mut stack = vec![tree.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("cannot list {}: {e}", dir.display()))?;
            let path = entry.path();
            let meta = entry
                .metadata()
                .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                let rel = path
                    .strip_prefix(tree)
                    .map_err(|e| format!("tree walk escaped {}: {e}", tree.display()))?;
                let Some(rel) = rel.to_str() else {
                    return Err(format!("tree path {} is not UTF-8", rel.display()));
                };
                let bytes = std::fs::read(&path)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
                inventory.insert(rel.to_string(), sha256_hex(&bytes));
            } else {
                return Err(format!(
                    "extracted tree contains a non-file non-directory entry {rel_display}",
                    rel_display = path.display()
                ));
            }
        }
    }
    Ok(inventory)
}
// ---------------------------------------------------------------------------
// Strict tar.gz extraction (tap tree as data only)
// ---------------------------------------------------------------------------

/// Erroring counting reader around the gzip stream: bounds ALL decoded
/// bytes (entry headers, file data, end-of-archive padding). A plain
/// `take` would masquerade the over-limit case as clean EOF and silently
/// accept a stream that is really a decompression bomb. The source is
/// deliberately a memory decoder: its reads cannot return Interrupted.
struct BoundedGzReader<'a> {
    inner: flate2::read::GzDecoder<&'a [u8]>,
    remaining: u64,
    consumed: u64,
}

impl<'a> BoundedGzReader<'a> {
    fn new(bytes: &'a [u8], limit: u64) -> Self {
        Self {
            inner: flate2::read::GzDecoder::new(bytes),
            remaining: limit,
            consumed: 0,
        }
    }

    /// Finish the bounded stream AFTER the tar reader stopped: consume
    /// every remaining decoded byte so hidden decompression padding after
    /// the tar end is rejected by the same bound, and gzip-level
    /// truncation or damage surfaces as a read error instead of a clean
    /// early EOF. A tar stream is 512-byte blocked; a decoded length off
    /// that grid means the tar itself was truncated.
    fn finish(&mut self) -> Result<(), String> {
        // Keep 64 KiB reads: decoder read size affects which damage or
        // byte-limit error is returned first.
        io::copy(
            &mut io::BufReader::with_capacity(64 * 1024, &mut *self),
            &mut io::sink(),
        )
        .map_err(|e| {
            if e.kind() == io::ErrorKind::InvalidData {
                e.to_string()
            } else {
                format!("archive stream is truncated or damaged after the tar end: {e}")
            }
        })?;
        if !self.consumed.is_multiple_of(512) {
            return Err(format!(
                "archive decoded {} bytes, which is not a whole number of 512-byte \
                 tar blocks; the tar stream is truncated",
                self.consumed
            ));
        }
        Ok(())
    }
}

impl Read for BoundedGzReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        let n64 = n as u64;
        if n64 > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "archive decompresses past the decoded-stream byte limit",
            ));
        }
        self.remaining -= n64;
        self.consumed += n64;
        Ok(n)
    }
}

/// Validate a `pax_global_header` (`XGlobalHeader`) member: bounded in
/// size and, record by record, ONLY `comment` metadata keys. `path`,
/// `linkpath`, or any other override key could move data around the
/// strict extraction rules, so those are refused.
fn validate_global_pax_header<R: Read>(entry: &mut tar::Entry<'_, R>) -> Result<(), String> {
    let size = entry
        .header()
        .size()
        .map_err(|e| format!("bad pax global header size: {e}"))?;
    if size > MAX_PAX_HEADER_BYTES {
        return Err(format!(
            "pax global header is {size} bytes; limit is {MAX_PAX_HEADER_BYTES}"
        ));
    }
    let mut buf = Vec::new();
    (&mut *entry)
        .take(size)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read the pax global header: {e}"))?;
    let text =
        std::str::from_utf8(&buf).map_err(|_| "pax global header is not UTF-8".to_string())?;
    for record in text.split('\n') {
        if record.is_empty() {
            continue;
        }
        let Some((len_str, rest)) = record.split_once(' ') else {
            return Err("pax global header has a malformed record".to_string());
        };
        if len_str.is_empty() || !len_str.bytes().all(|b| b.is_ascii_digit()) {
            return Err("pax global header has a malformed record length".to_string());
        }
        let Some((key, _)) = rest.split_once('=') else {
            return Err("pax global header record has no key".to_string());
        };
        if key != "comment" {
            return Err(format!(
                "pax global header carries key {key:?}; only comment metadata is allowed"
            ));
        }
    }
    Ok(())
}

/// Unpack a gzip tarball into `dest` under STRICT rules:
///
/// * every entry path must be relative, single common top-level
///   prefix (stripped), with no absolute/dotdot/dot/backslash/control
///   segments;
/// * only plain files and directories; symlink, hardlink, device, and
///   special entries are rejected;
/// * total unpacked bytes, file count, and per-file length are capped;
/// * case-insensitive collision checks cover every path and its
///   parents (a file may never shadow a directory or vice versa).
pub(super) fn extract_tar_gz(bytes: &[u8], dest: &Path) -> Result<usize, String> {
    extract_tar_gz_limited(bytes, dest, MAX_UNPACKED_BYTES, MAX_FILES, MAX_FILE_BYTES)
}

/// The real extraction under explicit limits. Production always goes
/// through [`extract_tar_gz`] (the full 512 MiB / 20 000 / 256 MiB
/// limits); unit tests call this private helper with SMALL limits to
/// exercise the decoded-byte bound cheaply (compressed header and
/// padding bombs). There is no production bypass: the public function
/// is the only caller with the real constants.
fn extract_tar_gz_limited(
    bytes: &[u8],
    dest: &Path,
    byte_limit: u64,
    max_files: usize,
    max_file_bytes: u64,
) -> Result<usize, String> {
    if bytes.len() > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "archive is {} bytes; limit is {MAX_ARCHIVE_BYTES}",
            bytes.len()
        ));
    }
    std::fs::create_dir_all(dest).map_err(|e| format!("cannot create {}: {e}", dest.display()))?;
    let mut bounded = BoundedGzReader::new(bytes, byte_limit);
    let mut tree = ExtractedTree::default();
    {
        let mut archive = tar::Archive::new(&mut bounded);
        archive.set_preserve_permissions(true);
        archive.set_unpack_xattrs(false);
        let mut entries = 0usize;
        for entry in archive
            .entries()
            .map_err(|e| format!("archive is damaged: {e}"))?
        {
            let mut entry = entry.map_err(|e| format!("archive is damaged: {e}"))?;
            entries = entries
                .checked_add(1)
                .ok_or_else(|| "archive entry count overflowed".to_string())?;
            if entries > max_files {
                return Err(format!(
                    "archive has more than {max_files} entries (files, directories, \
                     and metadata headers all count)"
                ));
            }
            let entry_type = entry.header().entry_type();
            if entry_type == tar::EntryType::XGlobalHeader {
                validate_global_pax_header(&mut entry)?;
                continue;
            }
            tree.unpack_entry(&mut entry, entry_type, dest, byte_limit, max_file_bytes)?;
        }
    }
    bounded.finish()?;
    if tree.file_count == 0 {
        return Err("archive contains no regular files".to_string());
    }
    Ok(tree.file_count)
}

#[derive(Default)]
struct ExtractedTree {
    prefix: Option<String>,
    files: BTreeSet<String>,
    dirs: BTreeSet<String>,
    total_bytes: u64,
    file_count: usize,
}

#[derive(Clone, Copy)]
struct ExtractLimits {
    bytes: u64,
    file_bytes: u64,
}

impl ExtractedTree {
    fn unpack_entry<R: Read>(
        &mut self,
        entry: &mut tar::Entry<'_, R>,
        entry_type: tar::EntryType,
        dest: &Path,
        byte_limit: u64,
        max_file_bytes: u64,
    ) -> Result<(), String> {
        let (path_str, relative_path) = self.entry_paths(entry)?;
        match entry_type {
            tar::EntryType::Directory if !relative_path.is_empty() => {
                self.create_directory(&relative_path, dest)
            }
            tar::EntryType::Directory => Ok(()),
            tar::EntryType::Regular => self.unpack_file(
                entry,
                &path_str,
                &relative_path,
                dest,
                ExtractLimits {
                    bytes: byte_limit,
                    file_bytes: max_file_bytes,
                },
            ),
            other => Err(format!(
                "archive entry {path_str:?} has type {other:?}; symlinks, hardlinks, \
                 devices, and special entries are refused"
            )),
        }
    }

    fn entry_paths<R: Read>(
        &mut self,
        entry: &mut tar::Entry<'_, R>,
    ) -> Result<(String, String), String> {
        if let Ok(Some(pax)) = entry.pax_extensions() {
            for ext in pax.flatten() {
                if let Ok(key) = ext.key()
                    && matches!(key, "path" | "linkpath" | "size")
                {
                    return Err(format!(
                        "archive entry carries a pax {key} override; path, link, or \
                         size metadata overrides are refused"
                    ));
                }
            }
        }
        let path = entry
            .path()
            .map_err(|e| format!("archive entry has no usable path: {e}"))?
            .into_owned();
        let Some(path_str) = path.to_str() else {
            return Err(format!("archive path {} is not UTF-8", path.display()));
        };
        let path_str = path_str.trim_end_matches('/');
        if path_str.is_empty() {
            return Err("archive has an empty path member".to_string());
        }
        if path_str.starts_with('/')
            || path_str.contains('\\')
            || path_str.chars().any(|c| c.is_ascii_control())
            || !path_str.split('/').all(safe_segment)
        {
            return Err(format!(
                "archive path {path_str:?} is not a safe relative tree path"
            ));
        }
        if !path_str.is_ascii() {
            return Err(format!(
                "archive path {path_str:?} contains non-ASCII characters; ASCII-only source \
                 filenames are required (Unicode case-fold and NFD/NFC collisions on macOS); \
                 rename the file in the upstream tap"
            ));
        }
        let (top, relative_path) = path_str.split_once('/').unwrap_or((path_str, ""));
        match &self.prefix {
            None => self.prefix = Some(top.to_string()),
            Some(prefix) if prefix == top => {}
            Some(prefix) => {
                return Err(format!(
                    "archive has multiple top-level prefixes ({prefix:?} and {top:?}); \
                     a single source tree prefix is required"
                ));
            }
        }
        Ok((path_str.to_string(), relative_path.to_string()))
    }

    fn create_directory(&mut self, path: &str, dest: &Path) -> Result<(), String> {
        let lower = path.to_ascii_lowercase();
        if self.files.contains(&lower) {
            return Err(format!(
                "case collision: directory {path:?} shadows an existing file"
            ));
        }
        self.register_ancestors(path)?;
        self.dirs.insert(lower);
        let target = dest.join(path);
        std::fs::create_dir_all(&target)
            .map_err(|e| format!("cannot create {}: {e}", target.display()))
    }

    fn unpack_file<R: Read>(
        &mut self,
        entry: &mut tar::Entry<'_, R>,
        archive_path: &str,
        relative_path: &str,
        dest: &Path,
        limits: ExtractLimits,
    ) -> Result<(), String> {
        if relative_path.is_empty() {
            return Err(format!(
                "archive member {archive_path:?} is a file at the prefix root; \
                 the prefix must be a directory"
            ));
        }
        let size = entry
            .header()
            .size()
            .map_err(|e| format!("bad entry size: {e}"))?;
        if size > limits.file_bytes {
            return Err(format!(
                "archive file {archive_path:?} is {size} bytes; per-file limit is {}",
                limits.file_bytes
            ));
        }
        self.total_bytes = self
            .total_bytes
            .checked_add(size)
            .ok_or_else(|| "archive total size overflowed".to_string())?;
        if self.total_bytes > limits.bytes {
            return Err(format!(
                "archive unpacks past the {} byte limit",
                limits.bytes
            ));
        }
        // Every regular file is one of the already counted, bounded entries.
        self.file_count += 1;
        let lower = relative_path.to_ascii_lowercase();
        if self.files.contains(&lower) || self.dirs.contains(&lower) {
            return Err(format!(
                "case collision: file {relative_path:?} shadows an existing entry"
            ));
        }
        self.register_ancestors(relative_path)?;
        self.files.insert(lower);
        let target = dest.join(relative_path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
        }
        entry
            .unpack(&target)
            .map(|_| ())
            .map_err(|e| format!("cannot unpack {archive_path:?}: {e}"))
    }

    fn register_ancestors(&mut self, path: &str) -> Result<(), String> {
        let mut current = path.to_ascii_lowercase();
        while let Some((parent, _)) = current.rsplit_once('/') {
            if self.files.contains(parent) {
                return Err(format!(
                    "archive path {path:?} descends into a file {parent:?}"
                ));
            }
            self.dirs.insert(parent.to_string());
            current = parent.to_string();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_enforces_strict_tree_rules() {
        fn tar_bytes(entries: &[(&str, &str, tar::EntryType)]) -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            for (path, data, kind) in entries {
                let mut header = tar::Header::new_gnu();
                if *kind == tar::EntryType::Directory {
                    header.set_entry_type(tar::EntryType::Directory);
                    header.set_size(0);
                    header.set_mode(0o755);
                    builder
                        .append_data(&mut header, path, std::io::empty())
                        .unwrap();
                } else {
                    header.set_entry_type(*kind);
                    header.set_size(data.len() as u64);
                    header.set_mode(0o644);
                    builder
                        .append_data(&mut header, path, data.as_bytes())
                        .unwrap();
                }
            }
            builder.into_inner().unwrap()
        }
        let gz = |raw: Vec<u8>| -> Vec<u8> {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut encoder, &raw).unwrap();
            encoder.finish().unwrap()
        };

        let dir = tempfile::tempdir().expect("tempdir");
        // Good: one prefix, stripped on extract.
        let good = gz(tar_bytes(&[
            ("tap-1/Casks/a.rb", "token 'a'", tar::EntryType::Regular),
            ("tap-1/Casks/sub", "", tar::EntryType::Directory),
            ("tap-1/Casks/sub/b.rb", "token 'b'", tar::EntryType::Regular),
        ]));
        let out = dir.path().join("good");
        assert_eq!(extract_tar_gz(&good, &out).unwrap(), 2);
        assert!(out.join("Casks/a.rb").is_file());
        assert!(out.join("Casks/sub/b.rb").is_file());

        // Two prefixes refused.
        let bad = gz(tar_bytes(&[
            ("p1/a.rb", "x", tar::EntryType::Regular),
            ("p2/b.rb", "x", tar::EntryType::Regular),
        ]));
        assert!(extract_tar_gz(&bad, &dir.path().join("two")).is_err());

        // Case collision refused.
        let bad = gz(tar_bytes(&[
            ("p/Casks/A.rb", "x", tar::EntryType::Regular),
            ("p/casks/a.rb", "x", tar::EntryType::Regular),
        ]));
        assert!(extract_tar_gz(&bad, &dir.path().join("case")).is_err());

        // Symlink refused.
        let raw = {
            let mut builder = tar::Builder::new(Vec::new());
            let data = b"target".to_vec();
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            builder
                .append_data(&mut header, "p/link", data.as_slice())
                .unwrap();
            builder.into_inner().unwrap()
        };
        assert!(extract_tar_gz(&gz(raw), &dir.path().join("link")).is_err());
    }
    #[test]
    fn extract_bounds_all_decoded_bytes_with_small_private_limits() {
        fn tar_bytes(entries: &[(&str, &str, tar::EntryType)]) -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            for (path, data, kind) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(*kind);
                header.set_size(data.len() as u64);
                header.set_mode(if *kind == tar::EntryType::Directory {
                    0o755
                } else {
                    0o644
                });
                builder
                    .append_data(&mut header, path, data.as_bytes())
                    .unwrap();
            }
            builder.into_inner().unwrap()
        }
        let gz = |raw: Vec<u8>| -> Vec<u8> {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut encoder, &raw).unwrap();
            encoder.finish().unwrap()
        };
        let dir = tempfile::tempdir().expect("tempdir");

        // Sanity: a small valid archive passes the small limits.
        let good = gz(tar_bytes(&[(
            "p/a.rb",
            "token 'a'",
            tar::EntryType::Regular,
        )]));
        assert!(
            extract_tar_gz_limited(&good, &dir.path().join("ok"), 64 * 1024, 100, 1024).is_ok()
        );

        // HEADER BOMB: no file data at all, but many directory headers
        // whose DECODED bytes pass the small limit — refused by the
        // counting reader, not masqueraded as clean EOF.
        let headers: Vec<(&str, &str, tar::EntryType)> = (0..200)
            .map(|i| {
                (
                    Box::leak(
                        format!("p/very/long/directory/path/segment/number/{i}").into_boxed_str(),
                    ) as &str,
                    "",
                    tar::EntryType::Directory,
                )
            })
            .collect();
        let bomb = gz(tar_bytes(&headers));
        let err =
            extract_tar_gz_limited(&bomb, &dir.path().join("header-bomb"), 8 * 1024, 100, 1024)
                .expect_err("header bomb");
        assert!(
            err.contains("decoded-stream") || err.contains("byte limit"),
            "unexpected error: {err}"
        );

        // PADDING BOMB: a valid small tar followed by a large run of
        // zero blocks AFTER the end-of-archive marker. The tar reader
        // stops at the marker; finishing the bounded stream rejects
        // the hidden decompression padding.
        let mut padded = tar_bytes(&[("p/a.rb", "x", tar::EntryType::Regular)]);
        padded.extend(std::iter::repeat_n(0u8, 512 * 64));
        let bomb = gz(padded);
        let err =
            extract_tar_gz_limited(&bomb, &dir.path().join("padding-bomb"), 8 * 1024, 100, 1024)
                .expect_err("padding bomb");
        assert!(
            err.contains("decoded-stream") || err.contains("byte limit"),
            "unexpected error: {err}"
        );

        // TRUNCATED gzip stream: cut the compressed bytes so the gzip
        // member never ends cleanly; the drain rejects it.
        let mut cut = gz(tar_bytes(&[(
            "p/a.rb",
            &"x".repeat(4096),
            tar::EntryType::Regular,
        )]));
        cut.truncate(cut.len() - 30);
        let err = extract_tar_gz_limited(&cut, &dir.path().join("cut"), 64 * 1024, 100, 8192)
            .expect_err("truncated gzip");
        assert!(
            err.contains("truncated") || err.contains("damaged"),
            "unexpected error: {err}"
        );

        // Metadata headers count as entries: many global PAX headers
        // past the small entry cap are refused even under a huge byte
        // limit.
        let mut builder = tar::Builder::new(Vec::new());
        for _ in 0..5 {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(0);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, "pax_global_header", std::io::empty())
                .unwrap();
        }
        let raw = builder.into_inner().unwrap();
        let err =
            extract_tar_gz_limited(&gz(raw), &dir.path().join("entries"), 1024 * 1024, 4, 1024)
                .expect_err("entry cap");
        assert!(err.contains("entries"), "unexpected error: {err}");
    }
    #[test]
    fn extract_rejects_pax_path_overrides_and_non_comment_globals() {
        fn raw_with_global(records: &str) -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(records.len() as u64);
            header.set_mode(0o644);
            builder
                .append_data(&mut header, "pax_global_header", records.as_bytes())
                .unwrap();
            let mut fh = tar::Header::new_gnu();
            fh.set_size(1);
            fh.set_mode(0o644);
            builder.append_data(&mut fh, "p/a.rb", &b"x"[..]).unwrap();
            builder.into_inner().unwrap()
        }
        let gz = |raw: Vec<u8>| -> Vec<u8> {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut encoder, &raw).unwrap();
            encoder.finish().unwrap()
        };
        let dir = tempfile::tempdir().expect("tempdir");
        // comment-only global header is allowed.
        let good = gz(raw_with_global("30 comment=refs/heads/master\n"));
        assert!(extract_tar_gz(&good, &dir.path().join("ok")).is_ok());
        // a path override in the global header is refused.
        let bad = gz(raw_with_global("18 path=../evil\n"));
        assert!(extract_tar_gz(&bad, &dir.path().join("bad1")).is_err());
    }

    #[test]
    fn extract_refuses_non_ascii_path_components() {
        fn tar_bytes(entries: &[(&str, &str)]) -> Vec<u8> {
            let mut builder = tar::Builder::new(Vec::new());
            for (path, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(data.len() as u64);
                header.set_mode(0o644);
                builder
                    .append_data(&mut header, path, data.as_bytes())
                    .unwrap();
            }
            builder.into_inner().unwrap()
        }
        let gz = |raw: Vec<u8>| -> Vec<u8> {
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            std::io::Write::write_all(&mut encoder, &raw).unwrap();
            encoder.finish().unwrap()
        };
        let dir = tempfile::tempdir().expect("tempdir");

        // NFC and NFD spellings of "Café.rb" are BOTH refused outright:
        // no Unicode normalization comparison is attempted.
        for name in ["p/Casks/Caf\u{e9}.rb", "p/Casks/Cafe\u{301}.rb"] {
            let bad = gz(tar_bytes(&[("p/Casks/plain.rb", "x"), (name, "y")]));
            let err = extract_tar_gz(&bad, &dir.path().join("nonascii"))
                .expect_err("non-ASCII path must be refused");
            assert!(err.contains("non-ASCII"), "unexpected error: {err}");
        }

        // Focused path-case control: the ASCII case check still fires
        // BEFORE any directory is created, and plain ASCII paths pass.
        let ok = gz(tar_bytes(&[("p/Casks/A.rb", "x")]));
        assert_eq!(extract_tar_gz(&ok, &dir.path().join("ascii")).unwrap(), 1);
    }
}
