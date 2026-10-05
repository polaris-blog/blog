//! Extension package handling: safe ZIP inspection, extraction and backup
//! creation.
//!
//! Threat model (everything below is enforced before any byte is written
//! outside the staging directory):
//! - **Zip Slip / path traversal** — entry names are validated segment by
//!   segment; backslashes, absolute paths, `..`, drive letters and Windows
//!   reserved device names are rejected outright.
//! - **Symlink attack** — entries carrying Unix symlink mode bits are rejected.
//! - **Zip bomb** — entry count and declared uncompressed sizes are checked
//!   up front, and a hard write budget is enforced while extracting (declared
//!   sizes can lie).
//! - **Executable upload** — packages are data + (for plugins) sandboxed
//!   Rhai source; common native executable/script extensions are rejected.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::result::ZipError;

use crate::error::{AppError, AppResult};

use super::ExtensionKind;

/// Limits applied to one uploaded package (sourced from
/// `[extensions.upload]`).
#[derive(Clone, Copy, Debug)]
pub struct PackageLimits {
    pub max_file_bytes: u64,
    pub max_uncompressed_bytes: u64,
    pub max_files: usize,
}

/// Where an archive's manifest lives and which entries it contains.
pub struct PackageInfo {
    pub kind: ExtensionKind,
    /// Single top-level directory inside the archive (`aurora.zip` usually
    /// contains `aurora/…`); `None` when the manifest sits at the archive root.
    pub root: Option<String>,
    /// Raw manifest file contents (validated before extraction).
    pub manifest_raw: String,
    pub entry_count: usize,
    pub total_uncompressed: u64,
}

/// File extensions never allowed inside a package. Plugins are sandboxed
/// Rhai *source* — a package carrying native executables or shell scripts is
/// either malicious or built for a runtime Polaris does not have.
const BLOCKED_EXTENSIONS: &[&str] = &[
    "exe", "dll", "so", "dylib", "bat", "cmd", "com", "scr", "msi", "ps1", "sh", "bash", "jar",
    "vbs", "wsf", "wsh", "app", "dmg", "deb", "rpm",
];

const RESERVED_WINDOWS_NAMES: &[&str] = &[
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8",
    "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

/// Validate one archive entry name and return its path segments.
/// Accepts only plain relative slash-separated paths (a trailing slash —
/// directory entries — is allowed).
fn safe_segments(name: &str) -> AppResult<Vec<&str>> {
    if name.contains('\\')
        || name.contains('\0')
        || name.bytes().any(|b| b < 0x20)
        || name.starts_with('/')
    {
        return Err(AppError::BadRequest(format!(
            "unsafe path in package: '{name}'"
        )));
    }
    let name = name.strip_suffix('/').unwrap_or(name);
    let mut out = Vec::new();
    for seg in name.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(AppError::BadRequest(format!(
                "unsafe path in package: '{name}'"
            )));
        }
        // Windows device names (CON, CON.txt, …) cannot be created safely.
        let stem = seg.split('.').next().unwrap_or("");
        if stem.len() <= 4 && RESERVED_WINDOWS_NAMES.contains(&stem.to_ascii_lowercase().as_str()) {
            return Err(AppError::BadRequest(format!(
                "reserved path in package: '{name}'"
            )));
        }
        out.push(seg);
    }
    Ok(out)
}

fn entry_extension(name: &str) -> &str {
    let base = name.rsplit('/').next().unwrap_or(name);
    match base.rsplit_once('.') {
        Some((_, ext)) => ext,
        None => "",
    }
}

fn zip_err(e: ZipError) -> AppError {
    AppError::BadRequest(format!("invalid ZIP package: {e}"))
}

fn open(path: &Path) -> AppResult<zip::ZipArchive<File>> {
    let file =
        File::open(path).map_err(|e| AppError::BadRequest(format!("cannot read package: {e}")))?;
    zip::ZipArchive::new(file).map_err(zip_err)
}

/// Streaming SHA-256 over a file (never loads it into memory).
pub fn sha256_file(path: &Path) -> AppResult<String> {
    let mut file =
        File::open(path).map_err(|e| AppError::BadRequest(format!("cannot read package: {e}")))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AppError::BadRequest(format!("hash failed: {e}")))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Inspect an archive without extracting: detect the extension kind from the
/// manifest it contains, locate the package root and enforce structural
/// limits.
pub fn inspect(path: &Path, limits: &PackageLimits) -> AppResult<PackageInfo> {
    let meta = std::fs::metadata(path)
        .map_err(|e| AppError::BadRequest(format!("cannot read package: {e}")))?;
    if meta.len() > limits.max_file_bytes {
        return Err(AppError::BadRequest(format!(
            "package is {} bytes (limit {})",
            meta.len(),
            limits.max_file_bytes
        )));
    }

    let mut archive = open(path)?;
    let count = archive.len();
    if count == 0 {
        return Err(AppError::BadRequest("package is empty".into()));
    }
    if count > limits.max_files {
        return Err(AppError::BadRequest(format!(
            "package has {count} entries (limit {})",
            limits.max_files
        )));
    }

    // First pass: names, sizes, symlink/executable scan.
    let mut names: Vec<String> = Vec::with_capacity(count);
    let mut total_uncompressed = 0u64;
    for i in 0..count {
        let f = archive.by_index(i).map_err(zip_err)?;
        let name = f.name().to_string();
        // Unix mode bits: reject anything that is a symlink (0o120000).
        if let Some(mode) = f.unix_mode()
            && mode & 0o170_000 == 0o120_000
        {
            return Err(AppError::BadRequest(format!(
                "packages must not contain symlinks ('{name}')"
            )));
        }
        safe_segments(&name)?;
        if !f.is_dir() {
            if BLOCKED_EXTENSIONS.contains(&entry_extension(&name).to_ascii_lowercase().as_str()) {
                return Err(AppError::BadRequest(format!(
                    "packages must not contain executables ('{name}')"
                )));
            }
            total_uncompressed = total_uncompressed
                .checked_add(f.size())
                .ok_or_else(|| AppError::BadRequest("package size overflow".into()))?;
        }
        names.push(name);
    }
    if total_uncompressed > limits.max_uncompressed_bytes {
        return Err(AppError::BadRequest(format!(
            "package expands to {} bytes (limit {})",
            total_uncompressed, limits.max_uncompressed_bytes
        )));
    }

    // Manifest location: `<root>/theme.toml`|`<root>/plugin.toml` where every
    // entry lives under the same single root, or a manifest at the archive
    // root itself (flat package).
    let mut top_dirs: Vec<&str> = names
        .iter()
        .filter_map(|n| {
            n.split_once('/')
                .filter(|(_, rest)| !rest.is_empty())
                .map(|(d, _)| d)
        })
        .collect();
    top_dirs.sort_unstable();
    top_dirs.dedup();
    let flat_files = names.iter().any(|n| !n.contains('/'));

    let (kind, root): (ExtensionKind, Option<String>) = if let Some(k) = names
        .iter()
        .find_map(|n| ExtensionKind::from_manifest_name(n))
    {
        // Manifest at archive root — flat package (subdirectories allowed).
        (k, None)
    } else if top_dirs.len() == 1 {
        let root = top_dirs[0].to_string();
        let theme_manifest = format!("{root}/theme.toml");
        let plugin_manifest = format!("{root}/plugin.toml");
        let kind = if names.iter().any(|n| n == &theme_manifest) {
            ExtensionKind::Theme
        } else if names.iter().any(|n| n == &plugin_manifest) {
            ExtensionKind::Plugin
        } else {
            return Err(AppError::BadRequest(
                "package contains neither theme.toml nor plugin.toml".into(),
            ));
        };
        // A rooted package must not also have stray files at the archive root.
        if flat_files {
            return Err(AppError::BadRequest(
                "package mixes a root directory with stray top-level files".into(),
            ));
        }
        (kind, Some(root))
    } else {
        return Err(AppError::BadRequest(
            "package contains neither theme.toml nor plugin.toml".into(),
        ));
    };

    // Second pass: read the manifest bytes.
    let manifest_name = match &root {
        Some(r) => format!("{r}/{}", kind.manifest_name()),
        None => kind.manifest_name().to_string(),
    };
    let idx = archive
        .index_for_name(&manifest_name)
        .ok_or_else(|| AppError::BadRequest(format!("{manifest_name} not found in package")))?;
    let mut raw = String::new();
    archive
        .by_index(idx)
        .map_err(zip_err)?
        .read_to_string(&mut raw)
        .map_err(|_| AppError::BadRequest("manifest is not valid UTF-8".into()))?;
    if raw.len() > 64 * 1024 {
        return Err(AppError::BadRequest("manifest is too large".into()));
    }

    Ok(PackageInfo {
        kind,
        root,
        manifest_raw: raw,
        entry_count: names.len(),
        total_uncompressed,
    })
}

/// Writer wrapper enforcing the remaining extraction budget even when the
/// archive's declared sizes lie.
struct LimitedWriter<W: Write> {
    inner: W,
    remaining: u64,
}

impl<W: Write> Write for LimitedWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if buf.len() as u64 > self.remaining {
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "package exceeds the uncompressed size limit while extracting",
            ));
        }
        self.remaining -= buf.len() as u64;
        self.inner.write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Extract an archive into `staging` (a fresh, empty directory). Returns the
/// path of the extracted package root (staging itself or the single root
/// directory inside it).
///
/// All safety checks from [`inspect`] are re-validated here — this function
/// must also be safe when called on its own.
pub fn extract(path: &Path, staging: &Path, limits: &PackageLimits) -> AppResult<PathBuf> {
    let mut archive = open(path)?;
    std::fs::create_dir_all(staging)
        .map_err(|e| AppError::BadRequest(format!("cannot create staging directory: {e}")))?;

    let count = archive.len();
    if count > limits.max_files {
        return Err(AppError::BadRequest(format!(
            "package has {count} entries (limit {})",
            limits.max_files
        )));
    }
    // `declared_remaining` is the cheap up-front guard against archives whose
    // *declared* sizes already exceed the budget; `remaining` is the real
    // write budget handed to `LimitedWriter`, because declared sizes can lie
    // (a zip bomb reports 1 KiB per entry and expands to gigabytes).
    let mut declared_remaining = limits.max_uncompressed_bytes;
    let mut remaining = limits.max_uncompressed_bytes;
    for i in 0..count {
        let mut f = archive.by_index(i).map_err(zip_err)?;
        let name = f.name().to_string();
        if let Some(mode) = f.unix_mode()
            && mode & 0o170_000 == 0o120_000
        {
            return Err(AppError::BadRequest(format!(
                "packages must not contain symlinks ('{name}')"
            )));
        }
        let segments = safe_segments(&name)?;
        if !f.is_dir() {
            if BLOCKED_EXTENSIONS.contains(&entry_extension(&name).to_ascii_lowercase().as_str()) {
                return Err(AppError::BadRequest(format!(
                    "packages must not contain executables ('{name}')"
                )));
            }
            let declared = f.size();
            if declared > declared_remaining {
                return Err(AppError::BadRequest(
                    "package expands beyond the uncompressed size limit".into(),
                ));
            }
            declared_remaining -= declared;
        }

        let mut dest = staging.to_path_buf();
        for seg in &segments {
            dest.push(seg);
        }
        // Belt and braces: the final path must stay inside staging.
        if dest.strip_prefix(staging).is_err() {
            return Err(AppError::BadRequest(format!(
                "unsafe path in package: '{name}'"
            )));
        }

        if f.is_dir() {
            std::fs::create_dir_all(&dest)
                .map_err(|e| AppError::BadRequest(format!("cannot create directory: {e}")))?;
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AppError::BadRequest(format!("cannot create directory: {e}")))?;
        }
        let file = File::create(&dest)
            .map_err(|e| AppError::BadRequest(format!("cannot create file '{name}': {e}")))?;
        let mut limited = LimitedWriter {
            inner: file,
            remaining,
        };
        let written = std::io::copy(&mut f, &mut limited)
            .map_err(|e| AppError::BadRequest(format!("extraction failed: {e}")))?;
        // Charge the *actual* bytes written, not the declared size.
        remaining = remaining.saturating_sub(written);
    }

    // The extracted package root.
    let entries: Vec<_> = std::fs::read_dir(staging)
        .map_err(|e| AppError::BadRequest(format!("cannot read staging directory: {e}")))?
        .flatten()
        .collect();
    match entries.len() {
        1 if entries[0].path().is_dir() => Ok(entries[0].path()),
        _ => Ok(staging.to_path_buf()),
    }
}

/// Zip a directory tree (used to back up the previous version before an
/// update). File names are stored with forward slashes; entry names are the
/// plain relative paths produced by walking the tree, so no user-controlled
/// naming is involved.
pub fn zip_dir(dir: &Path, out: &Path) -> AppResult<()> {
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::BadRequest(format!("cannot create backup directory: {e}")))?;
    }
    let file = File::create(out)
        .map_err(|e| AppError::BadRequest(format!("cannot create backup: {e}")))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    let base = dir.to_string_lossy().replace('\\', "/");
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)
            .map_err(|e| AppError::BadRequest(format!("cannot read directory: {e}")))?
            .flatten()
        {
            let path = entry.path();
            let rel = path.to_string_lossy().replace('\\', "/");
            let name = rel
                .strip_prefix(&format!("{}/", base.trim_end_matches('/')))
                .unwrap_or(&rel)
                .to_string();
            if path.is_dir() {
                stack.push(path);
                zip.add_directory(format!("{name}/"), options)
                    .map_err(zip_err)?;
            } else {
                zip.start_file(name, options).map_err(zip_err)?;
                let mut src = File::open(&path)
                    .map_err(|e| AppError::BadRequest(format!("cannot read backup source: {e}")))?;
                std::io::copy(&mut src, &mut zip)
                    .map_err(|e| AppError::BadRequest(format!("backup failed: {e}")))?;
            }
        }
    }
    zip.finish().map_err(zip_err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> PackageLimits {
        PackageLimits {
            max_file_bytes: 10 * 1024 * 1024,
            max_uncompressed_bytes: 10 * 1024 * 1024,
            max_files: 1000,
        }
    }

    fn write_zip(path: &Path, entries: &[(&str, &str)]) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        for (name, content) in entries {
            zip.start_file(name.to_string(), options).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap();
    }

    #[test]
    fn inspect_detects_kind_and_root() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("aurora.zip");
        write_zip(
            &zip_path,
            &[
                (
                    "aurora/theme.toml",
                    "id = \"aurora\"\nname = \"Aurora\"\nversion = \"1.0.0\"\nauthor = \"x\"\ndescription = \"d\"\nlicense = \"MIT\"\n",
                ),
                ("aurora/templates/index.html", "hi"),
            ],
        );
        let info = inspect(&zip_path, &limits()).unwrap();
        assert_eq!(info.kind, ExtensionKind::Theme);
        assert_eq!(info.root.as_deref(), Some("aurora"));

        let zip_path = dir.path().join("p.zip");
        write_zip(
            &zip_path,
            &[(
                "plugin.toml",
                "id = \"p\"\nname = \"P\"\nversion = \"1.0.0\"\nauthor = \"x\"\ndescription = \"d\"\nlicense = \"MIT\"\n",
            )],
        );
        let info = inspect(&zip_path, &limits()).unwrap();
        assert_eq!(info.kind, ExtensionKind::Plugin);
        assert_eq!(info.root, None);
    }

    #[test]
    fn rejects_traversal_backslash_and_oversized() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad.zip");
        write_zip(&bad, &[("theme.toml", "x"), ("../evil.txt", "x")]);
        assert!(inspect(&bad, &limits()).is_err());

        let bs = dir.path().join("bs.zip");
        write_zip(&bs, &[("theme.toml", "x"), ("..\\evil.txt", "x")]);
        assert!(inspect(&bs, &limits()).is_err());

        let big = dir.path().join("big.zip");
        let file = File::create(&big).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default().large_file(true);
        zip.start_file("theme.toml", options).unwrap();
        zip.write_all(&vec![0u8; 4096]).unwrap();
        zip.finish().unwrap();
        let small_limits = PackageLimits {
            max_file_bytes: 10 * 1024 * 1024,
            max_uncompressed_bytes: 2048,
            max_files: 1000,
        };
        assert!(inspect(&big, &small_limits).is_err());
    }

    #[test]
    fn rejects_executables_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("exe.zip");
        write_zip(&exe, &[("theme.toml", "x"), ("aurora/bin/tool.exe", "MZ")]);
        assert!(inspect(&exe, &limits()).is_err());

        // Symlink entry: the writer API cannot emit symlink mode bits, so
        // patch the central-directory record to Unix + S_IFLNK by hand.
        let sym = dir.path().join("sym.zip");
        let file = File::create(&sym).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("theme.toml", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"x").unwrap();
        zip.finish().unwrap();
        let mut bytes = std::fs::read(&sym).unwrap();
        let pos = bytes
            .windows(4)
            .position(|w| w == b"PK\x01\x02")
            .expect("central directory header");
        bytes[pos + 5] = 3; // version made by, high byte: Unix
        let mode: u32 = 0o120_777 << 16;
        bytes[pos + 38..pos + 42].copy_from_slice(&mode.to_le_bytes());
        std::fs::write(&sym, bytes).unwrap();
        assert!(inspect(&sym, &limits()).is_err());
    }

    #[test]
    fn extract_roundtrip_and_backup() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("aurora.zip");
        write_zip(
            &zip_path,
            &[
                (
                    "aurora/theme.toml",
                    "id = \"aurora\"\nname = \"Aurora\"\nversion = \"1.0.0\"\nauthor = \"x\"\ndescription = \"d\"\nlicense = \"MIT\"\n",
                ),
                ("aurora/templates/index.html", "hi"),
                ("aurora/assets/css/a.css", "body{}"),
            ],
        );
        let staging = dir.path().join("staging");
        let root = extract(&zip_path, &staging, &limits()).unwrap();
        assert!(root.ends_with("aurora"));
        assert_eq!(
            std::fs::read_to_string(root.join("templates/index.html")).unwrap(),
            "hi"
        );

        // Backup the extracted root and re-inspect it as a valid package.
        let backup = dir.path().join("backup.zip");
        zip_dir(&root, &backup).unwrap();
        let info = inspect(&backup, &limits()).unwrap();
        assert_eq!(info.kind, ExtensionKind::Theme);
    }

    /// A deflate bomb: 8 MiB of zeros compresses to a few KiB, so the
    /// archive file itself is tiny while the payload is not.
    fn write_bomb(path: &Path) {
        let file = File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("theme.toml", options).unwrap();
        zip.write_all(b"id = \"bomb\"\nname = \"B\"\nversion = \"1.0.0\"\nauthor = \"x\"\ndescription = \"d\"\nlicense = \"MIT\"\n").unwrap();
        zip.start_file("bomb/theme.toml", options).unwrap();
        zip.write_all(b"x").unwrap();
        zip.start_file("bomb/payload.bin", options).unwrap();
        zip.write_all(&vec![0u8; 8 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();
    }

    #[test]
    fn zip_bomb_is_stopped_by_the_write_budget() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("bomb.zip");
        write_bomb(&zip_path);
        // The on-disk archive stays well under max_file_bytes.
        assert!(std::fs::metadata(&zip_path).unwrap().len() < 256 * 1024);

        let tight = PackageLimits {
            max_file_bytes: 10 * 1024 * 1024,
            max_uncompressed_bytes: 1024 * 1024,
            max_files: 1000,
        };
        assert!(inspect(&zip_path, &tight).is_err());

        // `extract` must enforce the budget too, even when called standalone
        // — and must not leave an oversized payload on disk.
        let staging = dir.path().join("staging");
        assert!(extract(&zip_path, &staging, &tight).is_err());
        let written: u64 = walk_bytes(&staging);
        assert!(written <= 1024 * 1024, "extraction wrote {written} bytes");
    }

    /// Declared sizes are advisory: patch the central directory so an entry
    /// claims 1 KiB while the stream really expands past the budget.
    #[test]
    fn lying_declared_size_is_charged_by_actual_writes() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("lying.zip");
        {
            let file = File::create(&zip_path).unwrap();
            let mut zip = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("theme.toml", options).unwrap();
            zip.write_all(b"id = \"x\"\nname = \"X\"\nversion = \"1.0.0\"\nauthor = \"a\"\ndescription = \"d\"\nlicense = \"MIT\"\n").unwrap();
            zip.start_file("payload.bin", options).unwrap();
            zip.write_all(&vec![0u8; 2 * 1024 * 1024]).unwrap();
            zip.finish().unwrap();
        }
        // Shrink the declared uncompressed size of `payload.bin` (last
        // central-directory entry) to 1 KiB.
        let mut bytes = std::fs::read(&zip_path).unwrap();
        let mut pos = 0usize;
        while let Some(off) = bytes[pos..].windows(4).position(|w| w == b"PK\x01\x02") {
            let abs = pos + off;
            // Central-directory offsets: 28 = file name length, 46 = name.
            let name_len = u16::from_le_bytes([bytes[abs + 28], bytes[abs + 29]]) as usize;
            let name = String::from_utf8_lossy(&bytes[abs + 46..abs + 46 + name_len]).to_string();
            if name == "payload.bin" {
                bytes[abs + 24..abs + 28].copy_from_slice(&1024u32.to_le_bytes());
            }
            pos = abs + 4;
        }
        std::fs::write(&zip_path, bytes).unwrap();

        let tight = PackageLimits {
            max_file_bytes: 10 * 1024 * 1024,
            max_uncompressed_bytes: 512 * 1024,
            max_files: 1000,
        };
        let staging = dir.path().join("staging2");
        // The declared total (1 KiB) passes `inspect`…
        let res = extract(&zip_path, &staging, &tight);
        assert!(
            res.is_err(),
            "extraction must stop at the real write budget"
        );
        assert!(walk_bytes(&staging) <= 512 * 1024);
    }

    fn walk_bytes(dir: &Path) -> u64 {
        let mut total = 0u64;
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    total += std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                }
            }
        }
        total
    }

    #[test]
    fn multi_root_archive_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("multi.zip");
        write_zip(&zip_path, &[("a/theme.toml", "x"), ("b/other.txt", "x")]);
        assert!(inspect(&zip_path, &limits()).is_err());
    }
}
