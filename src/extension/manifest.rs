//! Theme / plugin manifests (`theme.toml` / `plugin.toml`).
//!
//! A manifest is the contract between an extension package and Polaris:
//! identity (`id`), display metadata, version, Polaris compatibility range,
//! dependencies and (for plugins) declared permissions. Validation is strict
//! at install time — an uploaded package with a broken manifest never
//! reaches disk.

use std::path::Path;

use semver::Version;
use serde::Deserialize;

use crate::error::{AppError, AppResult};

use super::ExtensionKind;

#[derive(Clone, Debug)]
pub struct ExtensionManifest {
    pub kind: ExtensionKind,
    pub id: String,
    pub name: String,
    pub version: Version,
    pub author: String,
    pub description: String,
    pub license: String,
    pub homepage: String,
    pub repository: String,
    pub minimum_polaris_version: Option<Version>,
    pub maximum_polaris_version: Option<Version>,
    /// (dependency id, version requirement) pairs, e.g. ("search-core", "^1.2").
    pub dependencies: Vec<(String, String)>,
    pub permissions: Vec<String>,
    /// Plugin only: entry script.
    pub entry: String,
    /// Plugin only: tables dropped by "remove plugin + data".
    pub uninstall_tables: Vec<String>,
}

/// Raw manifest table. Unknown keys (plugin route registrations etc.) are
/// ignored — the same file is read by the plugin runtime.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ManifestToml {
    id: String,
    name: String,
    version: String,
    author: String,
    description: String,
    license: String,
    homepage: String,
    repository: String,
    minimum_polaris_version: String,
    maximum_polaris_version: String,
    dependencies: toml::Table,
    permissions: Vec<String>,
    entry: String,
    uninstall_tables: Vec<String>,
}

/// Extension ids: `[a-z0-9-_]`, 1..=64 chars. They become directory names
/// (never file paths), so anything that could escape the install root —
/// `../`, `/`, `\`, dots — is rejected outright.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

/// Declared permission names: lowercase dotted identifiers (`posts.read`).
fn valid_permission(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 96
        && p.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-' || b == b'_'
        })
}

/// Parse `1.2` as `1.2.0` (two-component versions are tolerated, everything
/// else must be valid semver).
pub fn parse_version(s: &str) -> AppResult<Version> {
    let s = s.trim();
    if let Ok(v) = Version::parse(s) {
        return Ok(v);
    }
    let core = s.split(['-', '+']).next().unwrap_or(s);
    let parts: Vec<&str> = core.split('.').collect();
    if parts.len() == 2
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        && let Ok(v) = Version::parse(&format!("{}.{}", core.trim_end_matches('.'), "0"))
    {
        return Ok(v);
    }
    Err(AppError::BadRequest(format!(
        "invalid version '{s}' (expected semver like 1.2.0)"
    )))
}

impl ExtensionManifest {
    /// Compatibility error against the running Polaris version, if any.
    pub fn polaris_compat_error(&self, polaris: &Version) -> Option<String> {
        if let Some(min) = &self.minimum_polaris_version
            && polaris < min
        {
            return Some(format!("this extension requires Polaris >= {min}"));
        }
        if let Some(max) = &self.maximum_polaris_version
            && polaris > max
        {
            return Some(format!("this extension requires Polaris <= {max}"));
        }
        None
    }
}

/// Load and strictly validate the manifest inside an extension directory.
///
/// `fallback_id` is used when the manifest omits `id` (legacy packages where
/// the directory name — or, for plugins, `name` — *is* the id).
pub fn load_from_dir(
    kind: ExtensionKind,
    dir: &Path,
    fallback_id: Option<&str>,
) -> AppResult<ExtensionManifest> {
    let manifest_path = dir.join(kind.manifest_name());
    let raw = std::fs::read_to_string(&manifest_path).map_err(|_| {
        AppError::BadRequest(format!("manifest {} not found", kind.manifest_name()))
    })?;
    parse(kind, &raw, fallback_id)
}

/// Parse and validate raw manifest contents (used to validate a package
/// before anything is written to disk).
pub fn parse(
    kind: ExtensionKind,
    raw: &str,
    fallback_id: Option<&str>,
) -> AppResult<ExtensionManifest> {
    let t: ManifestToml = toml::from_str(raw)
        .map_err(|e| AppError::BadRequest(format!("invalid {}: {e}", kind.manifest_name())))?;

    // Identity: explicit id > directory name > legacy `name` field.
    let id = if !t.id.trim().is_empty() {
        t.id.trim().to_string()
    } else if let Some(fallback) = fallback_id.filter(|f| valid_id(f)) {
        fallback.to_string()
    } else if valid_id(t.name.trim()) {
        t.name.trim().to_string()
    } else {
        return Err(AppError::BadRequest(
            "manifest is missing a valid `id` (allowed: lowercase letters, digits, `-`, `_`)"
                .to_string(),
        ));
    };
    if !valid_id(&id) {
        return Err(AppError::BadRequest(format!("invalid extension id '{id}'")));
    }

    let name = if t.name.trim().is_empty() {
        id.clone()
    } else {
        t.name.trim().to_string()
    };
    let version = parse_version(&t.version)?;

    for (field, value) in [
        ("author", &t.author),
        ("description", &t.description),
        ("license", &t.license),
    ] {
        if value.trim().is_empty() {
            return Err(AppError::BadRequest(format!(
                "manifest is missing required field `{field}`"
            )));
        }
    }

    let mut dependencies = Vec::new();
    for (dep, req) in &t.dependencies {
        let dep = dep.trim().to_string();
        if !valid_id(&dep) {
            return Err(AppError::BadRequest(format!(
                "invalid dependency id '{dep}'"
            )));
        }
        let req = match req.as_str() {
            Some(r) => r.trim().to_string(),
            None => {
                return Err(AppError::BadRequest(format!(
                    "dependency '{dep}' must map to a version requirement string"
                )));
            }
        };
        semver::VersionReq::parse(&req).map_err(|_| {
            AppError::BadRequest(format!(
                "invalid version requirement '{req}' for dependency '{dep}'"
            ))
        })?;
        dependencies.push((dep, req));
    }

    let mut permissions = Vec::new();
    for p in &t.permissions {
        let p = p.trim();
        if !valid_permission(p) {
            return Err(AppError::BadRequest(format!(
                "invalid permission name '{p}'"
            )));
        }
        if !permissions.iter().any(|x| x == p) {
            permissions.push(p.to_string());
        }
    }

    let mut uninstall_tables = Vec::new();
    for tb in &t.uninstall_tables {
        let tb = tb.trim();
        if !valid_table_name(tb) {
            return Err(AppError::BadRequest(format!(
                "invalid uninstall table name '{tb}'"
            )));
        }
        uninstall_tables.push(tb.to_string());
    }

    let entry = if t.entry.trim().is_empty() {
        "main.rhai".to_string()
    } else {
        t.entry.trim().to_string()
    };
    // The entry must be a plain file name inside the plugin directory.
    if kind == ExtensionKind::Plugin
        && (entry.contains('/')
            || entry.contains('\\')
            || entry.contains("..")
            || entry.starts_with('.'))
    {
        return Err(AppError::BadRequest(format!(
            "invalid plugin entry '{entry}'"
        )));
    }

    let minimum_polaris_version = if t.minimum_polaris_version.trim().is_empty() {
        None
    } else {
        Some(parse_version(&t.minimum_polaris_version)?)
    };
    let maximum_polaris_version = if t.maximum_polaris_version.trim().is_empty() {
        None
    } else {
        Some(parse_version(&t.maximum_polaris_version)?)
    };

    Ok(ExtensionManifest {
        kind,
        id,
        name,
        version,
        author: t.author.trim().to_string(),
        description: t.description.trim().to_string(),
        license: t.license.trim().to_string(),
        homepage: t.homepage.trim().to_string(),
        repository: t.repository.trim().to_string(),
        minimum_polaris_version,
        maximum_polaris_version,
        dependencies,
        permissions,
        entry,
        uninstall_tables,
    })
}

/// Table names for `uninstall_tables` — identifiers only (they are spliced
/// into DROP statements, which cannot be parameterized).
pub fn valid_table_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        && name.first_byte_is_ascii_alpha()
}
trait FirstByteAlpha {
    fn first_byte_is_ascii_alpha(&self) -> bool;
}
impl FirstByteAlpha for str {
    fn first_byte_is_ascii_alpha(&self) -> bool {
        self.as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_manifest(dir: &Path, content: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("theme.toml"), content).unwrap();
    }

    #[test]
    fn valid_ids() {
        assert!(valid_id("aurora"));
        assert!(valid_id("dark-theme"));
        assert!(valid_id("search_enhancer"));
        assert!(valid_id("p2"));
        assert!(!valid_id("../../evil"));
        assert!(!valid_id("Aurora"));
        assert!(!valid_id("a b"));
        assert!(!valid_id(""));
        assert!(!valid_id("a/b"));
        assert!(!valid_id("a\\b"));
        assert!(!valid_id(".hidden"));
    }

    #[test]
    fn flexible_versions() {
        assert_eq!(parse_version("1.2.0").unwrap().to_string(), "1.2.0");
        assert_eq!(parse_version("1.2").unwrap().to_string(), "1.2.0");
        assert_eq!(parse_version(" 2.0.1 ").unwrap().to_string(), "2.0.1");
        assert!(parse_version("v1").is_err());
        assert!(parse_version("abc").is_err());
        assert!(parse_version("").is_err());
    }

    #[test]
    fn manifest_requires_core_fields() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "id = \"aurora\"\nname = \"Aurora\"\nversion = \"1.0.0\"\nauthor = \"x\"\n",
        );
        let err = load_from_dir(ExtensionKind::Theme, dir.path(), None).unwrap_err();
        assert!(err.message().contains("description"));

        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "id = \"aurora\"\nname = \"Aurora\"\nversion = \"1.0.0\"\nauthor = \"x\"\n\
             description = \"d\"\nlicense = \"MIT\"\nminimum_polaris_version = \"9.9.9\"\n",
        );
        let m = load_from_dir(ExtensionKind::Theme, dir.path(), None).unwrap();
        assert_eq!(m.name, "Aurora");
        assert!(
            m.polaris_compat_error(&Version::parse("0.1.0").unwrap())
                .is_some()
        );
        assert!(
            m.polaris_compat_error(&Version::parse("10.0.0").unwrap())
                .is_none()
        );
    }

    #[test]
    fn manifest_fallback_id_and_legacy_plugin_name() {
        // Legacy plugin.toml: `name` doubles as the id.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("plugin.toml"),
            "name = \"example\"\nversion = \"1.0.0\"\nauthor = \"a\"\ndescription = \"d\"\nlicense = \"MIT\"\n",
        )
        .unwrap();
        let m = load_from_dir(ExtensionKind::Plugin, dir.path(), None).unwrap();
        assert_eq!(m.id, "example");
        assert_eq!(m.name, "example");
    }

    #[test]
    fn invalid_dependency_requirement_rejected() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest(
            dir.path(),
            "id = \"a\"\nname = \"A\"\nversion = \"1.0.0\"\nauthor = \"x\"\ndescription = \"d\"\n\
             license = \"MIT\"\n[dependencies]\nsearch-core = \"not a version\"\n",
        );
        assert!(load_from_dir(ExtensionKind::Theme, dir.path(), None).is_err());
    }
}
