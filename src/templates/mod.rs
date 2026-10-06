//! Tera engine setup: shared filters, the embedded admin UI templates and the
//! built-in fallback theme.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use tera::{Context, Tera, Value};

use crate::error::AppResult;
use crate::plugins::PluginManager;

// ---------------------------------------------------------------------------
// Shared filters
// ---------------------------------------------------------------------------

pub fn register_common_filters(tera: &mut Tera) {
    tera.register_filter("date", filter_date);
    tera.register_filter("truncate_chars", filter_truncate_chars);
    crate::i18n::register(tera);
}

/// `{{ post.published_at | date(format="date") }}`
/// Presets: date, datetime, year, rfc3339, rfc822, http.
fn filter_date(value: &Value, args: &HashMap<String, Value>) -> tera::Result<Value> {
    let Some(ts) = value.as_i64() else {
        return Err(tera::Error::msg(
            "date filter expects an integer epoch timestamp",
        ));
    };
    let fmt = args.get("format").and_then(Value::as_str).unwrap_or("date");
    Ok(Value::String(crate::utils::time::format(ts, fmt)))
}

fn filter_truncate_chars(value: &Value, args: &HashMap<String, Value>) -> tera::Result<Value> {
    let n = args.get("n").and_then(Value::as_u64).unwrap_or(160) as usize;
    match value {
        Value::String(s) => Ok(Value::String(crate::markdown::truncate_chars(s, n))),
        _ => Err(tera::Error::msg("truncate_chars expects a string")),
    }
}

/// A Tera filter backed by a plugin script function.
struct PluginTeraFilter {
    manager: Arc<PluginManager>,
    plugin: String,
    fname: String,
}

impl tera::Filter for PluginTeraFilter {
    fn filter(&self, value: &Value, _args: &HashMap<String, Value>) -> tera::Result<Value> {
        let input = match value {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        match self
            .manager
            .call_plugin_str(&self.plugin, &self.fname, &input)
        {
            Some(out) => Ok(Value::String(out)),
            None => Err(tera::Error::msg(format!(
                "plugin filter `{}` failed",
                self.fname
            ))),
        }
    }
}

pub fn register_plugin_filters(tera: &mut Tera, plugins: &Arc<PluginManager>) {
    for (filter, plugin, fname) in plugins.filters() {
        tera.register_filter(
            &filter,
            PluginTeraFilter {
                manager: plugins.clone(),
                plugin,
                fname,
            },
        );
    }
}

// ---------------------------------------------------------------------------
// Admin UI (embedded — always available, zero JS)
// ---------------------------------------------------------------------------

static ADMIN_SOURCES: &[(&str, &str)] = &[
    ("base.html", include_str!("admin/base.html")),
    ("jobs.html", include_str!("admin/jobs.html")),
    ("job_detail.html", include_str!("admin/job_detail.html")),
    ("job_confirm.html", include_str!("admin/job_confirm.html")),
    ("login.html", include_str!("admin/login.html")),
    ("setup.html", include_str!("admin/setup.html")),
    ("dashboard.html", include_str!("admin/dashboard.html")),
    ("posts.html", include_str!("admin/posts.html")),
    ("post_edit.html", include_str!("admin/post_edit.html")),
    ("pages.html", include_str!("admin/pages.html")),
    ("page_edit.html", include_str!("admin/page_edit.html")),
    ("terms.html", include_str!("admin/terms.html")),
    ("comments.html", include_str!("admin/comments.html")),
    ("users.html", include_str!("admin/users.html")),
    ("user_edit.html", include_str!("admin/user_edit.html")),
    ("themes.html", include_str!("admin/themes.html")),
    ("plugins.html", include_str!("admin/plugins.html")),
    (
        "extensions_logs.html",
        include_str!("admin/extensions_logs.html"),
    ),
    ("config_form.html", include_str!("admin/config_form.html")),
    ("search.html", include_str!("admin/search.html")),
    ("settings.html", include_str!("admin/settings.html")),
    ("navigation.html", include_str!("admin/navigation.html")),
    ("profile.html", include_str!("admin/profile.html")),
    ("media.html", include_str!("admin/media.html")),
    ("media_detail.html", include_str!("admin/media_detail.html")),
    ("backups.html", include_str!("admin/backups.html")),
    (
        "backup_restore.html",
        include_str!("admin/backup_restore.html"),
    ),
];

pub fn admin_tera() -> &'static Tera {
    static ADMIN: OnceLock<Tera> = OnceLock::new();
    ADMIN.get_or_init(|| {
        let mut tera = Tera::default();
        for (name, src) in ADMIN_SOURCES {
            if let Err(e) = tera.add_raw_template(name, src) {
                panic!("invalid embedded admin template {name}: {e}");
            }
        }
        register_common_filters(&mut tera);
        tera
    })
}

pub fn render_admin(name: &str, ctx: &Context) -> AppResult<String> {
    admin_tera().render(name, ctx).map_err(Into::into)
}

/// Embedded admin stylesheet served at `/admin/static/style.css`.
pub const ADMIN_CSS: &str = include_str!("admin/admin.css");

/// Admin media script served at `/admin/static/media.js` (progressive
/// enhancement: uploads, drag-drop, paste, batch toolbar).
pub const ADMIN_MEDIA_JS: &str = include_str!("admin/media.js");

/// Admin extension upload script served at `/admin/static/extensions.js`
/// (progressive enhancement: drag-drop, upload progress, confirm guards).
pub const ADMIN_EXTENSIONS_JS: &str = include_str!("admin/extensions.js");

/// Admin navigation fade script served at `/admin/static/admin.js`
/// (progressive enhancement: fades content out before full-page navigation;
/// the incoming page fades in via CSS on `main.content`).
pub const ADMIN_JS: &str = include_str!("admin/admin.js");

/// Admin navigation editor script served at `/admin/static/navigation.js`
/// (progressive enhancement: add, remove and reorder custom link rows).
pub const ADMIN_NAVIGATION_JS: &str = include_str!("admin/navigation.js");

/// Admin Markdown editor (toolbar + server-rendered preview) for the
/// `textarea.content-editor` fields, served at `/admin/static/editor.js`.
pub const ADMIN_EDITOR_JS: &str = include_str!("admin/editor.js");

/// Setup wizard driver-switching script (shows only the fields relevant to
/// the selected database/cache driver), served at `/admin/static/setup.js`.
pub const ADMIN_SETUP_JS: &str = include_str!("admin/setup.js");

/// Built-in theme script served at `/static/js/theme.js` when the active
/// theme does not ship its own copy (covers the embedded fallback theme;
/// inline scripts are blocked by the CSP).
pub const THEME_JS: &str = include_str!("fallback/theme.js");

/// Syntax highlighter shared by the embedded fallback theme (`/static/js/
/// highlight.js` fallback) and the admin Markdown preview
/// (`/admin/static/highlight.js`). Themes may ship their own copy — a disk
/// file always wins.
pub const THEME_HL_JS: &str = include_str!("fallback/highlight.js");

// ---------------------------------------------------------------------------
// Fallback theme (used when no theme files exist on disk)
// ---------------------------------------------------------------------------

static FALLBACK_SOURCES: &[(&str, &str)] = &[
    ("base.html", include_str!("fallback/base.html")),
    ("index.html", include_str!("fallback/index.html")),
    ("post.html", include_str!("fallback/post.html")),
    ("page.html", include_str!("fallback/page.html")),
    ("category.html", include_str!("fallback/category.html")),
    ("tag.html", include_str!("fallback/tag.html")),
    ("search.html", include_str!("fallback/search.html")),
    ("404.html", include_str!("fallback/404.html")),
];

pub fn fallback_tera() -> Tera {
    let mut tera = Tera::default();
    for (name, src) in FALLBACK_SOURCES {
        if let Err(e) = tera.add_raw_template(name, src) {
            panic!("invalid embedded fallback template {name}: {e}");
        }
    }
    register_common_filters(&mut tera);
    tera
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every embedded template must parse (both template sets).
    #[test]
    fn embedded_templates_parse() {
        let _ = admin_tera();
        let _ = fallback_tera();
    }

    /// Every admin template file on disk must be listed in `ADMIN_SOURCES` —
    /// an unregistered template renders as a 500 at request time.
    #[test]
    fn admin_sources_cover_all_template_files() {
        let listed: Vec<&str> = ADMIN_SOURCES.iter().map(|(name, _)| *name).collect();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/templates/admin");
        let mut files: Vec<String> = std::fs::read_dir(&dir)
            .expect("admin template dir exists")
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "html"))
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        files.sort();
        for f in files {
            assert!(
                listed.contains(&f.as_str()),
                "src/templates/admin/{f} is not registered in ADMIN_SOURCES"
            );
        }
    }
}
