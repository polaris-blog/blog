pub mod client_ip;
pub mod cookies;
pub mod fs;
pub mod hash;
pub mod lock;
pub mod slug;
pub mod time;
pub mod xml;

use std::path::Path;

/// Recursively copy a directory. Used by `theme install` / `plugin install`.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}
