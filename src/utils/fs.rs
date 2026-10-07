//! Filesystem helpers.
//!
//! Deployment layouts routinely put the data directory (uploads, staging,
//! backups) and the application directories (themes, plugins) on **different
//! filesystems** — Docker volumes vs image layers above all. `std::fs::rename`
//! fails with `EXDEV` (os error 18, "Cross-device link") in that case, so
//! every move between the two needs a copy fallback.

use std::fs;
use std::io;
use std::path::Path;

/// `true` when the error is EXDEV ("Cross-device link").
fn is_exdev(e: &io::Error) -> bool {
    e.raw_os_error() == Some(18) || e.kind() == io::ErrorKind::CrossesDevices
}

/// Recursively copy a directory tree (files, dirs and symlinks as files).
fn copy_dir_all(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let target = to.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Move a directory, falling back to copy + delete when source and target
/// live on different filesystems (EXDEV). `to` must not exist.
pub fn rename_dir_or_copy(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if is_exdev(&e) => {
            copy_dir_all(from, to)?;
            fs::remove_dir_all(from)
        }
        Err(e) => Err(e),
    }
}

/// Move a file, falling back to copy + delete on EXDEV.
pub fn rename_file_or_copy(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(e) if is_exdev(&e) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_dir_moves_and_falls_back() {
        let tmp = tempfile::TempDir::new().unwrap();
        let from = tmp.path().join("from");
        fs::create_dir_all(from.join("nested")).unwrap();
        fs::write(from.join("nested/a.txt"), b"hello").unwrap();
        let to = tmp.path().join("to");
        rename_dir_or_copy(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(
            fs::read_to_string(to.join("nested/a.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn rename_file_moves_and_falls_back() {
        let tmp = tempfile::TempDir::new().unwrap();
        let from = tmp.path().join("a.bin");
        fs::write(&from, b"data").unwrap();
        let to = tmp.path().join("b.bin");
        rename_file_or_copy(&from, &to).unwrap();
        assert!(!from.exists());
        assert_eq!(fs::read(to).unwrap(), b"data");
    }
}
