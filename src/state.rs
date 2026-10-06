use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::auth::{LoginLimiter, SessionStore};
use crate::cache::CacheManager;
use crate::config::Config;
use crate::config_schema::Permission;
use crate::config_store::{ConfigManager, SaveOutcome, plugin_ns, theme_ns};
use crate::db::{Db, migrate};
use crate::error::AppResult;
use crate::plugins::PluginManager;
use crate::repositories;
use crate::themes::ThemeManager;
use crate::utils::time;

pub type App = Arc<AppState>;

// ---------------------------------------------------------------------------
// Settings (config file provides defaults, the DB overrides at runtime)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Settings {
    cache: RwLock<HashMap<String, String>>,
}

impl Settings {
    pub fn from_map(map: HashMap<String, String>) -> Self {
        Self {
            cache: RwLock::new(map),
        }
    }

    pub fn get(&self, key: &str) -> Option<String> {
        crate::utils::lock::read(&self.cache).get(key).cloned()
    }

    pub fn get_str(&self, key: &str, default: &str) -> String {
        self.get(key)
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    pub fn get_bool(&self, key: &str, default: bool) -> bool {
        self.get(key).map(|v| v == "true").unwrap_or(default)
    }

    pub fn get_usize(&self, key: &str, default: usize) -> usize {
        self.get(key)
            .and_then(|v| v.parse().ok())
            .unwrap_or(default)
    }

    pub async fn set(&self, db: &Db, key: &str, value: &str) -> AppResult<()> {
        repositories::settings::set(db, key, value).await?;
        crate::utils::lock::write(&self.cache).insert(key.to_string(), value.to_string());
        Ok(())
    }

    pub async fn set_many(&self, db: &Db, values: &HashMap<String, String>) -> AppResult<()> {
        for (k, v) in values {
            self.set(db, k, v).await?;
        }
        Ok(())
    }

    pub fn plugins_enabled(&self) -> Vec<String> {
        self.get("plugins.enabled")
            .and_then(|v| serde_json::from_str::<Vec<String>>(&v).ok())
            .unwrap_or_default()
    }

    /// Replace the whole in-memory snapshot (used after a backup restore
    /// rewrote the `settings` table).
    pub fn reload(&self, map: HashMap<String, String>) {
        let mut cache = crate::utils::lock::write(&self.cache);
        cache.clear();
        cache.extend(map);
    }
}

// ---------------------------------------------------------------------------
// Rendered-Markdown cache (invalidated by content update or plugin reload)
// ---------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MdKey {
    pub kind: u8,
    pub id: i64,
    pub updated: i64,
    pub generation: u64,
}

#[derive(Default)]
pub struct MdCache {
    map: RwLock<HashMap<MdKey, Arc<String>>>,
}

impl MdCache {
    pub fn get(&self, key: &MdKey) -> Option<Arc<String>> {
        crate::utils::lock::read(&self.map).get(key).cloned()
    }

    pub fn put(&self, key: MdKey, html: Arc<String>) {
        let mut map = crate::utils::lock::write(&self.map);
        if map.len() > 512 {
            map.clear(); // crude but bounded and cheap
        }
        map.insert(key, html);
    }
}

// ---------------------------------------------------------------------------
// Full-page response cache lives in `crate::cache` (ns "pagecache"): entries
// are keyed by (host, path) and invalidated wholesale via a namespace
// version bump on every content mutation.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Application state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub db: Db,
    pub config: Config,
    pub settings: Settings,
    pub theme: ThemeManager,
    pub plugins: Arc<PluginManager>,
    pub sessions: SessionStore,
    pub md_cache: MdCache,
    pub limiter: LoginLimiter,
    /// Rate limiter for public comment submission (per-IP).
    pub comment_limiter: LoginLimiter,
    pub started_at: i64,
    /// Layered cache: response cache + object cache + stats + plugin cache.
    pub cache: Arc<CacheManager>,
    /// Schema-backed configuration for themes and plugins.
    pub configs: Arc<ConfigManager>,
    /// Full-text search (provider abstraction + result cache).
    pub search: Arc<crate::search::SearchService>,
    /// Media management (storage provider + configuration).
    pub media: crate::media::MediaService,
    pub scheduler: Arc<crate::scheduler::Scheduler>,
    /// True until the first user account exists — drives the setup wizard.
    needs_setup: AtomicBool,
}

impl AppState {
    /// Whether the first-run setup wizard should be offered.
    pub fn needs_setup(&self) -> bool {
        self.needs_setup.load(Ordering::Relaxed)
    }

    /// Mark the instance as installed (first user created).
    pub fn set_setup_done(&self) {
        self.needs_setup.store(false, Ordering::Relaxed);
    }
}

impl AppState {
    pub async fn init(cfg: Config) -> anyhow::Result<App> {
        cfg.security.validate()?;
        cfg.cache.validate()?;
        let db = Db::connect(&cfg.database).await?;
        if cfg.database.auto_migrate {
            migrate::run(&db).await?;
        }
        let defaults: Vec<(&str, String)> = vec![
            ("site.title", cfg.site.title.clone()),
            ("site.description", cfg.site.description.clone()),
            ("site.base_url", cfg.site.base_url.clone()),
            ("site.posts_per_page", cfg.site.posts_per_page.to_string()),
            ("comments.enabled", cfg.comments.enabled.to_string()),
            ("comments.moderate", cfg.comments.moderate.to_string()),
            ("theme.active", cfg.theme.active.clone()),
            ("plugins.enabled", "[]".to_string()),
        ];
        let default_refs: Vec<(&str, &str)> =
            defaults.iter().map(|(k, v)| (*k, v.as_str())).collect();
        repositories::settings::ensure_defaults(&db, &default_refs).await?;

        // Extensions that pre-date the registry (shipped with the deployment,
        // restored from backup) are registered once so verify and the admin UI
        // see a complete picture. Failure is never fatal.
        let installer = crate::extension::installer::ExtensionInstaller::new(db.clone(), &cfg);
        match installer.seed_registry().await {
            Ok(n) if n > 0 => tracing::info!(count = n, "registered pre-existing extensions"),
            Ok(_) => {}
            Err(e) => tracing::warn!(error = %e, "extension registry seeding failed"),
        }

        // Encryption key material for sensitive configuration values:
        // explicit config value wins, then a persisted random secret
        // (generated on first boot), so restarts decrypt old values.
        let secret = if !cfg.security.secret.is_empty() {
            cfg.security.secret.clone()
        } else {
            match repositories::settings::get(&db, "security.secret").await? {
                Some(s) if !s.is_empty() => s,
                _ => {
                    let s = crate::utils::cookies::random_token(32);
                    repositories::settings::set(&db, "security.secret", &s).await?;
                    s
                }
            }
        };

        if secret.trim().is_empty() || secret.trim().eq_ignore_ascii_case("CHANGE_ME") {
            anyhow::bail!(
                "persisted security.secret is invalid; configure a random instance key and migrate encrypted settings"
            );
        }

        let settings = Settings::from_map(repositories::settings::all(&db).await?);
        // Site-wide UI language for theme/admin template rendering.
        crate::i18n::init(settings.get("site.locale").as_deref());
        // Runtime cache settings (Admin → Settings) override the file
        // configuration, applied on restart like every other setting.
        let mut cache_cfg = cfg.cache.clone();
        if let Some(v) = settings.get("cache.enabled") {
            cache_cfg.enabled = v == "true";
        }
        if let Some(v) = settings.get("cache.driver") {
            let d = v.trim();
            if d.eq_ignore_ascii_case("memory") || d.eq_ignore_ascii_case("redis") {
                cache_cfg.driver = d.to_string();
            }
        }
        if let Some(v) = settings.get("cache.redis.url") {
            let u = v.trim();
            if !u.is_empty() {
                cache_cfg.redis.url = u.to_string();
            }
        }
        if let Some(password) = settings.get("cache.redis.password") {
            let password = password.trim();
            if !password.is_empty() {
                cache_cfg.redis.url =
                    crate::config::redis_url_with_password(&cache_cfg.redis.url, password);
            }
        }
        if let Some(v) = settings.get("cache.redis.namespace") {
            let n = v.trim();
            if !n.is_empty() {
                cache_cfg.redis.namespace = n.to_string();
            }
        }
        if let Err(e) = cache_cfg.validate() {
            tracing::warn!(
                error = %e,
                "runtime cache settings are invalid — falling back to the file configuration"
            );
            cache_cfg = cfg.cache.clone();
        }
        // First-run detection: the setup wizard is offered until a user exists.
        let needs_setup = repositories::users::count(&db).await.unwrap_or(0) == 0;

        let cache = CacheManager::build(&cache_cfg).await;
        let active = settings.get_str("theme.active", &cfg.theme.active);
        let enabled_plugins = settings.plugins_enabled();

        // Theme/plugin configuration must be loaded before the plugin
        // engines are built (init(config) receives effective values).
        let configs = Arc::new(ConfigManager::new(db.clone(), secret));
        for (name, value) in repositories::settings::all(&db).await? {
            if (name.starts_with("theme.") || name.starts_with("plugin."))
                && value.starts_with("enc:")
            {
                configs
                    .validate_stored_secret(&value)
                    .map_err(|error| anyhow::anyhow!("{name}: {error}"))?;
            }
        }
        if let Err(e) = configs.load_theme(Path::new(&cfg.theme.dir), &active).await {
            tracing::warn!(theme = active, error = %e, "theme config schema failed to load");
        }
        for name in &enabled_plugins {
            if let Err(e) = configs.load_plugin(Path::new(&cfg.plugin.dir), name).await {
                tracing::warn!(plugin = name, error = %e, "plugin config schema failed to load");
            }
        }

        let scheduler = crate::scheduler::Scheduler::new(
            Arc::new(crate::repositories::jobs::SqlJobRepository::new(db.clone())),
            cfg.scheduler.clone(),
        )?;
        let plugins = Arc::new(PluginManager::with_scheduler(
            Path::new(&cfg.plugin.dir),
            &enabled_plugins,
            Some(cache.plugin_cache_handle()),
            configs.clone(),
            Some(Arc::downgrade(&scheduler)),
        ));
        scheduler.set_provider(Arc::new(crate::plugins::jobs::PluginJobs(Arc::downgrade(
            &plugins,
        ))));
        let theme = ThemeManager::new(Path::new(&cfg.theme.dir), &active, plugins.clone());

        let search = Arc::new(crate::search::SearchService::new(
            cfg.search.clone(),
            db.dialect(),
        ));
        if let Err(e) = search.provider_probe(&db).await {
            tracing::warn!(error = %e, "search index unavailable — run `polaris migrate` / `polaris search rebuild`");
        }

        let media = crate::media::MediaService::build(&cfg.media)?;

        let app = Arc::new(Self {
            db,
            config: cfg,
            settings,
            theme,
            plugins,
            sessions: SessionStore::new(),
            md_cache: MdCache::default(),
            limiter: LoginLimiter::new(),
            comment_limiter: LoginLimiter::new(),
            started_at: time::now(),
            cache,
            configs,
            search,
            media,
            scheduler,
            needs_setup: AtomicBool::new(needs_setup),
        });
        crate::services::jobs::register(&app)?;
        Ok(app)
    }

    /// Invalidate cached public pages (rendered HTML). Used by mutations
    /// that change page content but not post/feed objects (comments).
    pub async fn bump_content(&self) {
        self.cache.invalidate_pages().await;
    }

    /// Invalidate every content namespace (posts, pages, terms, feeds) and
    /// the rendered-HTML cache. Call after any content mutation.
    pub async fn invalidate_content(&self) {
        self.cache.invalidate_content().await;
    }

    /// A ready-to-use backup service bound to this instance's database,
    /// configuration and media storage.
    pub fn backup(&self) -> crate::backup::BackupService<'_> {
        crate::backup::BackupService::new(&self.db, &self.config, self.media.storage())
            .expect("backup storage provider is always available for local")
    }

    // -- convenience accessors (settings override config) -------------------

    pub fn site_title(&self) -> String {
        self.settings.get_str("site.title", &self.config.site.title)
    }

    pub fn site_description(&self) -> String {
        self.settings
            .get_str("site.description", &self.config.site.description)
    }

    pub fn base_url(&self) -> String {
        self.settings
            .get_str("site.base_url", &self.config.site.base_url)
    }

    pub fn posts_per_page(&self) -> usize {
        self.settings
            .get_usize("site.posts_per_page", self.config.site.posts_per_page)
            .max(1)
    }

    pub fn comments_enabled(&self) -> bool {
        self.settings
            .get_bool("comments.enabled", self.config.comments.enabled)
    }

    pub fn comments_moderate(&self) -> bool {
        self.settings
            .get_bool("comments.moderate", self.config.comments.moderate)
    }

    // -- runtime mutations ---------------------------------------------------

    pub async fn set_active_theme(&self, name: &str) -> AppResult<()> {
        self.theme.activate(name)?;
        self.settings.set(&self.db, "theme.active", name).await?;
        // Load the newly active theme's configuration namespace.
        if let Err(e) = self
            .configs
            .load_theme(Path::new(&self.config.theme.dir), name)
            .await
        {
            tracing::warn!(theme = name, error = %e, "theme config schema failed to load");
        }
        self.invalidate_content().await;
        Ok(())
    }

    pub async fn set_plugins_enabled(&self, names: &[String]) -> AppResult<()> {
        let json = serde_json::to_string(names).unwrap_or_else(|_| "[]".into());
        self.settings
            .set(&self.db, "plugins.enabled", &json)
            .await?;
        // Keep config namespaces in sync with the enabled set.
        let plugin_dir = Path::new(&self.config.plugin.dir);
        for name in names {
            if self.configs.entry(&plugin_ns(name)).is_none()
                && let Err(e) = self.configs.load_plugin(plugin_dir, name).await
            {
                tracing::warn!(plugin = name, error = %e, "plugin config schema failed to load");
            }
        }
        for ns in self.configs.loaded_namespaces() {
            if let Some(name) = ns.strip_prefix("plugin.")
                && !names.iter().any(|n| n == name)
            {
                self.configs.unload_namespace(&ns);
            }
        }
        self.plugins.reload(names);
        // Plugin-provided Tera filters may have changed.
        self.theme.reload_current();
        self.invalidate_content().await;
        Ok(())
    }

    // -- schema-backed theme/plugin configuration ------------------------------

    /// Validate + persist theme configuration, then invalidate the render
    /// caches (values like `site_title` or `accent_color` affect every page)
    /// and notify plugin `on_config_changed` hooks.
    pub async fn save_theme_config(
        &self,
        theme: &str,
        input: &HashMap<String, String>,
        actor: Permission,
    ) -> Result<SaveOutcome, String> {
        let ns = theme_ns(theme);
        // Refresh from disk first so schema edits are picked up.
        self.configs
            .load_theme(Path::new(&self.config.theme.dir), theme)
            .await
            .map_err(|e| format!("cannot load theme schema: {e}"))?;
        let out = self.configs.save(&ns, input, actor).await?;
        if !out.changes.is_empty() {
            self.invalidate_content().await;
            for change in &out.changes {
                self.plugins
                    .hook_void("on_config_changed", &change.to_event_map());
            }
        }
        Ok(out)
    }

    /// Validate + persist plugin configuration, invalidate rendered pages
    /// (plugin output is often embedded in them) and fire `on_config_changed`.
    pub async fn save_plugin_config(
        &self,
        plugin: &str,
        input: &HashMap<String, String>,
        actor: Permission,
    ) -> Result<SaveOutcome, String> {
        let ns = plugin_ns(plugin);
        self.configs
            .load_plugin(Path::new(&self.config.plugin.dir), plugin)
            .await
            .map_err(|e| format!("cannot load plugin schema: {e}"))?;
        let out = self.configs.save(&ns, input, actor).await?;
        if !out.changes.is_empty() {
            self.bump_content().await;
            for change in &out.changes {
                self.plugins
                    .hook_void("on_config_changed", &change.to_event_map());
            }
        }
        Ok(out)
    }

    /// Effective theme configuration visible to templates (secrets masked).
    pub fn theme_config_json(&self) -> serde_json::Value {
        self.configs
            .values_json(&theme_ns(&self.theme.current_name()), false)
    }

    /// Run pending maintenance (scheduled posts).
    pub async fn promote_scheduled(&self) -> AppResult<u64> {
        let n = repositories::posts::promote_scheduled(&self.db, time::now()).await?;
        // Search index rows for scheduled posts exist with visible = 0 — flip
        // them in bulk. This runs unconditionally, not only when `n > 0`:
        // gating it on `n` means one failed pass leaves those posts invisible
        // forever, because later passes find nothing left to promote and
        // never retry. The statement is a cheap indexed UPDATE that matches
        // nothing once the index is already consistent.
        if let Err(e) = self.search.promote_visible(&self.db, time::now()).await {
            tracing::warn!(error = %e, "search visibility update failed after promotion");
        }
        if n > 0 {
            self.invalidate_content().await;
        }
        Ok(n)
    }
}
