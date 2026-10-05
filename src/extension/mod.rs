//! Extension infrastructure: themes and plugins are both *extensions* —
//! ZIP packages with a manifest (`theme.toml` / `plugin.toml`) that are
//! uploaded, validated, installed and managed through one pipeline.
//!
//! ```text
//! Admin / CLI → installer.rs → package.rs (safe ZIP) → manifest.rs (contract)
//!                      ↓                repositories/extensions.rs (registry)
//!              themes/<id>/  or  plugins/<id>/      (files on disk)
//! ```
//!
//! Design rules:
//! - Everything works offline — a ZIP file is the only input needed.
//! - Extraction happens into `data/tmp/extensions/` and only ever reaches
//!   `themes/` / `plugins/` after every validation passed (atomic rename).
//! - The core stays light: marketplace, signatures and remote updates are
//!   deliberately out of scope (see `ExtensionSecurityConfig`).

pub mod installer;
pub mod manifest;
pub mod package;

pub use installer::{ExtensionInstaller, InstallAction, InstallOutcome, UninstallOutcome};
pub use manifest::ExtensionManifest;

/// What kind of extension a package is — decided by which manifest file the
/// archive contains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExtensionKind {
    Theme,
    Plugin,
}

impl ExtensionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Theme => "theme",
            Self::Plugin => "plugin",
        }
    }

    /// Manifest file name inside the package root.
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::Theme => "theme.toml",
            Self::Plugin => "plugin.toml",
        }
    }

    /// Parse `"theme"`/`"themes"`/`"plugin"`/`"plugins"` (case-insensitive).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "theme" | "themes" => Some(Self::Theme),
            "plugin" | "plugins" => Some(Self::Plugin),
            _ => None,
        }
    }

    /// Parse from a manifest file name (`theme.toml` / `plugin.toml`).
    pub fn from_manifest_name(name: &str) -> Option<Self> {
        match name {
            "theme.toml" => Some(Self::Theme),
            "plugin.toml" => Some(Self::Plugin),
            _ => None,
        }
    }
}
