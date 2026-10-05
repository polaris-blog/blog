//! Theme system. Themes live on disk (`themes/<name>/`) and are never compiled
//! into the binary — they can be hot-swapped at runtime.
//!
//! ```text
//! themes/default/
//! ├── theme.toml
//! ├── templates/   (index, post, page, category, tag, 404)
//! └── static/      (css/, js/, images/ — served at /static/*)
//! ```

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use serde::Deserialize;
use tera::Tera;

use crate::error::{AppError, AppResult};
use crate::plugins::PluginManager;
use crate::templates;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct ThemeMeta {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
}

pub struct ThemeRuntime {
    /// Directory name (the theme id).
    pub dir_name: String,
    pub meta: ThemeMeta,
    pub tera: Tera,
    pub static_dir: PathBuf,
    /// True when the embedded fallback templates are in use.
    pub is_fallback: bool,
}

impl ThemeRuntime {
    pub fn display_name(&self) -> &str {
        if self.meta.name.is_empty() {
            &self.dir_name
        } else {
            &self.meta.name
        }
    }
}

pub struct ThemeManager {
    themes_dir: PathBuf,
    plugins: Arc<PluginManager>,
    current: RwLock<Arc<ThemeRuntime>>,
}

impl ThemeManager {
    pub fn new(themes_dir: &Path, active: &str, plugins: Arc<PluginManager>) -> Self {
        let runtime = load_runtime(themes_dir, active, &plugins)
            .or_else(|_| load_runtime(themes_dir, "default", &plugins))
            .unwrap_or_else(|_| embedded_fallback());
        Self {
            themes_dir: themes_dir.to_path_buf(),
            plugins,
            current: RwLock::new(Arc::new(runtime)),
        }
    }

    pub fn current(&self) -> Arc<ThemeRuntime> {
        crate::utils::lock::read(&self.current).clone()
    }

    pub fn current_name(&self) -> String {
        self.current().dir_name.clone()
    }

    /// Load a theme and swap it in as the active theme (hot switch).
    pub fn activate(&self, name: &str) -> AppResult<()> {
        let runtime = load_runtime(&self.themes_dir, name, &self.plugins)?;
        *crate::utils::lock::write(&self.current) = Arc::new(runtime);
        Ok(())
    }

    /// Rebuild the current theme's Tera instance (e.g. after plugin changes).
    pub fn reload_current(&self) {
        let name = self.current_name();
        if let Err(e) = self.activate(&name) {
            tracing::error!(theme = name, error = %e, "theme reload failed");
        }
    }

    /// All themes available on disk: `(dir_name, meta, is_active)`. The
    /// directory name is the theme id used for activation and config
    /// namespaces; `meta.name` is display-only.
    pub fn list(&self) -> Vec<(String, ThemeMeta, bool)> {
        let active = self.current_name();
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.themes_dir) {
            for entry in entries.flatten() {
                if !entry.path().join("theme.toml").is_file() {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                let meta = std::fs::read_to_string(entry.path().join("theme.toml"))
                    .ok()
                    .and_then(|raw| toml::from_str::<ThemeMeta>(&raw).ok())
                    .unwrap_or_default();
                let is_active = dir_name == active;
                out.push((dir_name, meta, is_active));
            }
        }
        out.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        out
    }
}

fn load_runtime(
    themes_dir: &Path,
    name: &str,
    plugins: &Arc<PluginManager>,
) -> AppResult<ThemeRuntime> {
    // Reject suspicious names (path traversal).
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        return Err(AppError::BadRequest("invalid theme name".into()));
    }
    let theme_dir = themes_dir.join(name);
    if !theme_dir.is_dir() {
        return Err(AppError::NotFound(format!("theme '{name}' not found")));
    }
    let meta: ThemeMeta = std::fs::read_to_string(theme_dir.join("theme.toml"))
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default();

    let mut tera = match Tera::new(&glob_path(&theme_dir, "templates/**/*")) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(theme = name, error = %e, "theme templates failed to load, using fallback");
            templates::fallback_tera()
        }
    };
    templates::register_common_filters(&mut tera);
    templates::register_plugin_filters(&mut tera, plugins);

    Ok(ThemeRuntime {
        dir_name: name.to_string(),
        meta,
        tera,
        static_dir: theme_dir.join("static"),
        is_fallback: false,
    })
}

fn embedded_fallback() -> ThemeRuntime {
    ThemeRuntime {
        dir_name: "(builtin)".into(),
        meta: ThemeMeta {
            name: "Polaris Built-in".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            author: "Polaris".into(),
            description: "Embedded fallback theme".into(),
        },
        tera: templates::fallback_tera(),
        static_dir: PathBuf::new(),
        is_fallback: true,
    }
}

/// Build a glob pattern with forward slashes (works on Windows too).
fn glob_path(dir: &Path, rest: &str) -> String {
    let dir = dir.to_string_lossy().replace('\\', "/");
    format!("{dir}/{rest}")
}
