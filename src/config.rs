use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub server: ServerConfig,
    pub database: DatabaseConfig,
    pub theme: ThemeConfig,
    pub plugin: PluginConfig,
    pub security: SecurityConfig,
    pub site: SiteConfig,
    pub comments: CommentsConfig,
    pub cache: CacheConfig,
    pub search: SearchConfig,
    pub media: MediaConfig,
    pub extensions: ExtensionsConfig,
    pub backup: BackupConfig,
    pub scheduler: crate::scheduler::SchedulerConfig,
    /// File the configuration was loaded from — set by the CLI, not the file.
    /// The first-run setup wizard writes environment changes back to it.
    #[serde(skip)]
    pub config_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: String,
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 3000,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct DatabaseConfig {
    pub driver: String,
    pub url: String,
    pub max_connections: u32,
    pub min_connections: u32,
    pub auto_migrate: bool,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            driver: "sqlite".into(),
            url: "data/polaris.db".into(),
            max_connections: 10,
            min_connections: 0,
            auto_migrate: true,
        }
    }
}

impl DatabaseConfig {
    pub fn driver_normalized(&self) -> &str {
        match self.driver.to_ascii_lowercase().as_str() {
            "mysql" | "mariadb" => "mysql",
            "postgres" | "postgresql" | "pg" => "postgres",
            _ => "sqlite",
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub active: String,
    pub dir: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            active: "default".into(),
            dir: "themes".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct PluginConfig {
    pub dir: String,
}

impl Default for PluginConfig {
    fn default() -> Self {
        Self {
            dir: "plugins".into(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SecurityConfig {
    pub secret: String,
    /// Exact proxy IP addresses allowed to supply X-Forwarded-For.
    pub trusted_proxies: Vec<std::net::IpAddr>,
    pub session_ttl_hours: i64,
    /// Send the `Secure` attribute on session cookies.
    ///
    /// `None` (the default) means "auto": on when `site.base_url` is an
    /// `https://` URL, off otherwise — so plain-HTTP local development keeps
    /// working while HTTPS deployments stop exposing the session cookie to a
    /// stray HTTP request.
    pub secure_cookies: Option<bool>,
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            secret: String::new(),
            trusted_proxies: Vec::new(),
            session_ttl_hours: 72,
            secure_cookies: None,
        }
    }
}

impl SecurityConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.secret.trim().eq_ignore_ascii_case("CHANGE_ME")
            || (!self.secret.is_empty() && self.secret.trim().is_empty())
        {
            anyhow::bail!(
                "security.secret is a placeholder; generate a random key with `openssl rand -hex 32` or leave it empty for automatic generation"
            );
        }
        if self.session_ttl_hours <= 0 || self.session_ttl_hours.checked_mul(3600).is_none() {
            anyhow::bail!("security.session_ttl_hours must be positive and fit in seconds");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SiteConfig {
    pub title: String,
    pub description: String,
    pub base_url: String,
    pub posts_per_page: usize,
}

impl Default for SiteConfig {
    fn default() -> Self {
        Self {
            title: "Polaris".into(),
            description: "A fast, lightweight blog powered by Polaris".into(),
            base_url: String::new(),
            posts_per_page: 10,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CommentsConfig {
    pub enabled: bool,
    pub moderate: bool,
}

impl Default for CommentsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            moderate: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Cache
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CacheConfig {
    pub enabled: bool,
    /// "memory" (default, zero external dependencies) or "redis"
    /// (requires a build with `--features redis`).
    pub driver: String,
    /// Fallback TTL (seconds) for namespaces without a dedicated entry.
    pub default_ttl: u64,
    pub memory: MemoryCacheConfig,
    pub redis: RedisCacheConfig,
    pub ttl: CacheTtlConfig,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            driver: "memory".into(),
            default_ttl: 300,
            memory: MemoryCacheConfig::default(),
            redis: RedisCacheConfig::default(),
            ttl: CacheTtlConfig::default(),
        }
    }
}

impl CacheConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.enabled && self.driver.eq_ignore_ascii_case("redis") {
            self.redis.validate()?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MemoryCacheConfig {
    /// Maximum number of live entries (LRU eviction beyond this).
    /// Eviction is entry-count based; approximate memory usage is reported
    /// in the admin stats but not enforced.
    pub max_entries: usize,
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        Self { max_entries: 4096 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct RedisCacheConfig {
    pub url: String,
    /// Required for Redis: stable site identifier shared by this site's
    /// instances, distinct from every other site/environment on the server.
    pub namespace: String,
    /// Advisory only: the backend multiplexes commands over a single
    /// auto-reconnecting connection, which covers blog-scale traffic.
    pub pool_size: u32,
}

impl Default for RedisCacheConfig {
    fn default() -> Self {
        Self {
            url: "redis://127.0.0.1:6379".into(),
            namespace: String::new(),
            pool_size: 8,
        }
    }
}

impl RedisCacheConfig {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.namespace.is_empty()
                && self.namespace.len() <= 128
                && self
                    .namespace
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
            "cache.redis.namespace must contain 1–128 ASCII letters, digits, '.', '_' or '-'"
        );
        Ok(())
    }
}

/// Per-namespace TTLs in seconds (safety net — invalidation on writes is
/// explicit and immediate; TTL only bounds staleness in unusual cases).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct CacheTtlConfig {
    pub post: u64,
    pub page: u64,
    pub category: u64,
    pub tag: u64,
    /// Homepage / post lists, and the rendered-HTML response cache.
    pub homepage: u64,
    pub posts: u64,
    pub rss: u64,
    pub atom: u64,
    pub sitemap: u64,
    /// Search results and suggestions (`[search.cache]` namespace).
    pub search: u64,
}

impl Default for CacheTtlConfig {
    fn default() -> Self {
        Self {
            post: 600,
            page: 600,
            category: 300,
            tag: 300,
            homepage: 60,
            posts: 60,
            rss: 300,
            atom: 300,
            sitemap: 600,
            search: 60,
        }
    }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

/// Per-field relevance weights. SQLite applies them directly as bm25()
/// arguments; PostgreSQL maps relative weight ranks onto tsvector weight
/// letters (A > B > C > D); MySQL uses them as a title/tags boost on top of
/// the natural-language relevance score.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SearchWeights {
    pub title: f64,
    pub excerpt: f64,
    pub tags: f64,
    pub category: f64,
    pub author: f64,
    pub content: f64,
}

impl Default for SearchWeights {
    fn default() -> Self {
        Self {
            title: 5.0,
            excerpt: 3.0,
            tags: 3.0,
            category: 2.0,
            author: 1.0,
            content: 1.0,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SearchCacheConfig {
    pub enabled: bool,
}

impl Default for SearchCacheConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct SearchAnalyticsConfig {
    /// Off by default (privacy-first). When enabled, only the normalized
    /// query text and aggregate counters are stored — never IPs, user
    /// agents or identities.
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    pub enabled: bool,
    /// "auto"/"database" → the dialect's native full-text engine
    /// (SQLite FTS5 / MySQL FULLTEXT / PostgreSQL FTS). Named external
    /// providers are accepted and, when unavailable in this build, fall
    /// back to database search unless `fallback = false`.
    pub provider: String,
    pub fallback: bool,
    pub default_per_page: u32,
    pub max_per_page: u32,
    /// Minimum number of characters for a query to be executed.
    pub minimum_query_length: usize,
    pub suggestion_limit: usize,
    pub highlight: bool,
    /// Text-search language config for PostgreSQL tsvector/tsquery
    /// ("simple" is language-neutral and CJK-safe by exact token).
    pub language: String,
    pub weights: SearchWeights,
    pub cache: SearchCacheConfig,
    pub analytics: SearchAnalyticsConfig,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: "auto".into(),
            fallback: true,
            default_per_page: 10,
            max_per_page: 50,
            minimum_query_length: 2,
            suggestion_limit: 10,
            highlight: true,
            language: "simple".into(),
            weights: SearchWeights::default(),
            cache: SearchCacheConfig::default(),
            analytics: SearchAnalyticsConfig::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------

/// Parse a human size string ("20MB", "500MiB", "1.5GB", "2M") into bytes.
/// Binary units (KiB/MiB/GiB) and decimal-looking units (KB/MB/GB) both use
/// 1024-based multipliers — the convention users expect from upload limits.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num, unit) = s.split_at(s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len()));
    let num: f64 = num.trim().parse().ok()?;
    let mult = match unit.trim().to_ascii_uppercase().as_str() {
        "" | "B" => 1u64,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        _ => return None,
    };
    Some((num * mult as f64) as u64)
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaStorageConfig {
    /// "local" (default, zero dependencies). S3-compatible providers
    /// (s3/r2/minio) are planned as an optional build feature.
    pub provider: String,
    /// Root directory for local storage.
    pub dir: String,
}

impl Default for MediaStorageConfig {
    fn default() -> Self {
        Self {
            provider: "local".into(),
            dir: "data/media".into(),
        }
    }
}

/// Per-kind upload limits. Enforced at the HTTP layer (request rejected as
/// soon as a field exceeds its cap — never buffered to disk first).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaSizeLimits {
    pub image: String,
    pub video: String,
    pub audio: String,
    pub document: String,
    pub archive: String,
    pub other: String,
}

impl Default for MediaSizeLimits {
    fn default() -> Self {
        Self {
            image: "20MB".into(),
            video: "500MB".into(),
            audio: "100MB".into(),
            document: "50MB".into(),
            archive: "50MB".into(),
            other: "20MB".into(),
        }
    }
}

impl MediaSizeLimits {
    pub fn for_kind(&self, kind: &str) -> u64 {
        let raw = match kind {
            "image" => &self.image,
            "video" => &self.video,
            "audio" => &self.audio,
            "document" => &self.document,
            "archive" => &self.archive,
            _ => &self.other,
        };
        parse_size(raw).unwrap_or(20 * 1024 * 1024)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaUploadConfig {
    /// Global cap for a single upload (HTTP body limit for upload routes).
    pub max_file_size: String,
    /// Reuse the existing record when an upload's SHA-256 already exists.
    pub deduplicate: bool,
    /// Extra extensions an admin chose to allow beyond the built-in
    /// whitelist (e.g. ["doc", "docx"]). Magic bytes cannot be verified for
    /// these; they are served as application/octet-stream.
    pub extra_types: Vec<String>,
    pub limits: MediaSizeLimits,
}

impl Default for MediaUploadConfig {
    fn default() -> Self {
        Self {
            max_file_size: "20MB".into(),
            deduplicate: true,
            extra_types: Vec::new(),
            limits: MediaSizeLimits::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaImagesConfig {
    pub generate_thumbnails: bool,
    /// "" keeps each upload's source format; "webp"/"jpeg"/"png" re-encodes
    /// raster uploads (GIF is never converted — animation would be lost).
    pub preferred_format: String,
    pub sizes: MediaImageSizes,
    pub quality: MediaImageQuality,
}

impl Default for MediaImagesConfig {
    fn default() -> Self {
        Self {
            generate_thumbnails: true,
            preferred_format: String::new(),
            sizes: MediaImageSizes::default(),
            quality: MediaImageQuality::default(),
        }
    }
}

/// Only the sizes present here are generated (others cost disk + CPU).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaImageSizes {
    pub thumb: u32,
    pub small: u32,
    pub medium: u32,
    pub large: u32,
}

impl Default for MediaImageSizes {
    fn default() -> Self {
        Self {
            thumb: 240,
            small: 480,
            medium: 960,
            large: 1920,
        }
    }
}

impl MediaImageSizes {
    pub fn as_pairs(&self) -> Vec<(&'static str, u32)> {
        vec![
            ("thumb", self.thumb),
            ("small", self.small),
            ("medium", self.medium),
            ("large", self.large),
        ]
        .into_iter()
        .filter(|(_, px)| *px > 0)
        .collect()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaImageQuality {
    pub jpeg: u8,
    pub webp: u8,
}

impl Default for MediaImageQuality {
    fn default() -> Self {
        Self { jpeg: 85, webp: 85 }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaExifConfig {
    /// Re-encode JPEG uploads without EXIF (GPS, camera serial, device
    /// info are dropped; orientation is baked into the pixels first).
    pub strip: bool,
}

impl Default for MediaExifConfig {
    fn default() -> Self {
        Self { strip: true }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaSvgConfig {
    pub enabled: bool,
    /// Strict allow-list sanitizer (scripts, event handlers and external
    /// references are removed). Upload is rejected when sanitization fails.
    pub sanitize: bool,
}

impl Default for MediaSvgConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            sanitize: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct MediaCdnConfig {
    pub enabled: bool,
    /// Absolute base URL; media URLs are prefixed with it when enabled.
    pub url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct MediaConfig {
    pub enabled: bool,
    pub storage: MediaStorageConfig,
    pub upload: MediaUploadConfig,
    pub images: MediaImagesConfig,
    pub exif: MediaExifConfig,
    pub svg: MediaSvgConfig,
    pub cdn: MediaCdnConfig,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            storage: MediaStorageConfig::default(),
            upload: MediaUploadConfig::default(),
            images: MediaImagesConfig::default(),
            exif: MediaExifConfig::default(),
            svg: MediaSvgConfig::default(),
            cdn: MediaCdnConfig::default(),
        }
    }
}

impl MediaConfig {
    pub fn provider_normalized(&self) -> &str {
        match self.storage.provider.to_ascii_lowercase().as_str() {
            "s3" | "r2" | "minio" => "s3",
            _ => "local",
        }
    }

    /// Largest single-upload cap across all kinds (used as the HTTP body
    /// limit so oversized requests are rejected before buffering).
    pub fn max_upload_bytes(&self) -> u64 {
        let l = &self.upload.limits;
        let per_kind = [
            l.for_kind("image"),
            l.for_kind("video"),
            l.for_kind("audio"),
            l.for_kind("document"),
            l.for_kind("archive"),
            l.for_kind("other"),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        let global = parse_size(&self.upload.max_file_size)
            .unwrap_or(20 * 1024 * 1024)
            .max(1024 * 1024);
        // The global cap always applies; per-kind caps may exceed it only
        // for their own kind, so the body limit must cover the largest.
        per_kind.max(global)
    }
}

// ---------------------------------------------------------------------------
// Backup & restore
// ---------------------------------------------------------------------------

/// Scheduled (automatic) backup settings. State (`last_run`) lives in the
/// settings table, so schedules survive restarts on every dialect.
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct BackupAutoConfig {
    pub enabled: bool,
    /// Minimum hours between automatic backups (checked once per minute by
    /// the background scheduler).
    pub interval_hours: u64,
    /// What an automatic backup contains: "full" | "database" | "media".
    pub kind: String,
    /// How many scheduled backups to keep (oldest beyond this are pruned).
    pub keep: usize,
}

impl Default for BackupAutoConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            interval_hours: 24,
            kind: "full".into(),
            keep: 5,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct BackupConfig {
    /// Where backup archives live (local provider).
    pub dir: String,
    /// Staging area for in-progress archives and uploaded backups.
    pub tmp_dir: String,
    /// Cap for an uploaded backup archive (HTTP body limit on the upload
    /// route and a sanity check for restore).
    pub max_upload_size: String,
    /// Sum of all uncompressed entry sizes inside an archive (zip-bomb guard).
    pub max_uncompressed_size: String,
    /// Maximum number of entries in an archive.
    pub max_files: usize,
    /// Retention for scheduled backups (newest N kept).
    pub keep_auto: usize,
    pub auto: BackupAutoConfig,
}

impl Default for BackupConfig {
    fn default() -> Self {
        Self {
            dir: "data/backups".into(),
            tmp_dir: "data/tmp/backups".into(),
            max_upload_size: "2GB".into(),
            max_uncompressed_size: "4GB".into(),
            max_files: 20_000,
            keep_auto: 5,
            auto: BackupAutoConfig::default(),
        }
    }
}

impl BackupConfig {
    pub fn max_upload_bytes(&self) -> u64 {
        parse_size(&self.max_upload_size).unwrap_or(2 * 1024 * 1024 * 1024)
    }

    pub fn max_uncompressed_bytes(&self) -> u64 {
        parse_size(&self.max_uncompressed_size).unwrap_or(4 * 1024 * 1024 * 1024)
    }
}

// ---------------------------------------------------------------------------
// Extensions (themes & plugins: upload / install / verify)
// ---------------------------------------------------------------------------

/// Upload limits for extension packages. Enforced before extraction so a
/// 100 KB zip can never expand into gigabytes on disk (zip bomb guard).
#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ExtensionUploadConfig {
    /// Cap for a single uploaded .zip (HTTP body limit on upload routes).
    pub max_file_size: String,
    /// Sum of all uncompressed entry sizes inside the archive.
    pub max_uncompressed_size: String,
    /// Maximum number of files (entries) in the archive.
    pub max_files: usize,
}

impl Default for ExtensionUploadConfig {
    fn default() -> Self {
        Self {
            max_file_size: "20MB".into(),
            max_uncompressed_size: "100MB".into(),
            max_files: 5000,
        }
    }
}

impl ExtensionUploadConfig {
    pub fn max_file_bytes(&self) -> u64 {
        parse_size(&self.max_file_size).unwrap_or(20 * 1024 * 1024)
    }

    pub fn max_uncompressed_bytes(&self) -> u64 {
        parse_size(&self.max_uncompressed_size).unwrap_or(100 * 1024 * 1024)
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct ExtensionSecurityConfig {
    /// Reserved for the (future, optional) signed-package workflow. When
    /// true, packages without a trusted signature will be rejected —
    /// verification itself is not implemented yet, so shipping this enabled
    /// would brick uploads; it is parsed and surfaced for forward
    /// compatibility only.
    pub require_signature: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct ExtensionsConfig {
    pub upload: ExtensionUploadConfig,
    pub security: ExtensionSecurityConfig,
    /// Staging area for uploaded packages (never inside themes/ or plugins/).
    pub tmp_dir: String,
    /// Version backups taken before updates/downgrades.
    pub backup_dir: String,
}

impl Default for ExtensionsConfig {
    fn default() -> Self {
        Self {
            upload: ExtensionUploadConfig::default(),
            security: ExtensionSecurityConfig::default(),
            tmp_dir: "data/tmp/extensions".into(),
            backup_dir: "data/backups/extensions".into(),
        }
    }
}

impl Config {
    /// Load config from a TOML file, then apply environment / CLI overrides.
    /// `overrides` maps dotted paths like `server.port` to raw values.
    pub fn load(path: Option<&Path>, overrides: &HashMap<String, String>) -> anyhow::Result<Self> {
        let mut root: toml::Value = match path {
            Some(p) if p.exists() => {
                let raw = std::fs::read_to_string(p)?;
                toml::from_str::<toml::Value>(&raw)?
            }
            Some(p) => {
                tracing::warn!(path = %p.display(), "config file not found, using defaults");
                toml::Value::Table(Default::default())
            }
            None => toml::Value::Table(Default::default()),
        };
        for (key, value) in overrides {
            apply_override(&mut root, key, value);
        }
        let cfg: Config = root.try_into()?;
        cfg.security.validate()?;
        cfg.cache.validate()?;
        Ok(cfg)
    }

    /// Collect `POLARIS_*` environment overrides, e.g.
    /// `POLARIS_SERVER_PORT=8080` -> `server.port = "8080"`.
    pub fn env_overrides() -> HashMap<String, String> {
        let mut out = HashMap::new();
        for (k, v) in std::env::vars() {
            let Some(rest) = k.strip_prefix("POLARIS_") else {
                continue;
            };
            if rest.is_empty() {
                continue;
            }
            // First underscore splits section from key: SITE_BASE_URL -> site.base_url
            let dotted = match rest.split_once('_') {
                Some((section, key)) => {
                    format!(
                        "{}.{}",
                        section.to_ascii_lowercase(),
                        key.to_ascii_lowercase()
                    )
                }
                None => rest.to_ascii_lowercase(),
            };
            out.insert(dotted, v);
        }
        out
    }
}

fn apply_override(root: &mut toml::Value, dotted: &str, raw: &str) {
    let mut parts = dotted.split('.').peekable();
    let mut cur = root;
    while let Some(part) = parts.next() {
        if !matches!(cur, toml::Value::Table(_)) {
            *cur = toml::Value::Table(Default::default());
        }
        let table = cur.as_table_mut().unwrap();
        if parts.peek().is_none() {
            table.insert(part.to_string(), parse_toml_value(raw));
            return;
        }
        cur = table
            .entry(part.to_string())
            .or_insert_with(|| toml::Value::Table(Default::default()));
    }
}

fn parse_toml_value(raw: &str) -> toml::Value {
    if raw.trim_start().starts_with('[')
        && let Ok(parsed) = toml::from_str::<toml::Table>(&format!("value = {raw}"))
        && let Some(value @ toml::Value::Array(_)) = parsed.get("value")
    {
        return value.clone();
    }
    if raw == "true" {
        return toml::Value::Boolean(true);
    }
    if raw == "false" {
        return toml::Value::Boolean(false);
    }
    if let Ok(i) = raw.parse::<i64>() {
        return toml::Value::Integer(i);
    }
    if let Ok(f) = raw.parse::<f64>() {
        return toml::Value::Float(f);
    }
    toml::Value::String(raw.to_string())
}

/// Default config file path used by the CLI.
pub fn default_config_path() -> PathBuf {
    PathBuf::from("polaris.toml")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let cfg = Config::load(None, &HashMap::new()).unwrap();
        assert_eq!(cfg.server.port, 3000);
        assert_eq!(cfg.database.driver_normalized(), "sqlite");
        assert!(cfg.database.auto_migrate);
        assert_eq!(cfg.theme.active, "default");
        assert!(cfg.comments.enabled);
    }

    #[test]
    fn redis_requires_an_explicit_safe_site_namespace() {
        for namespace in [
            "", " ", "site:*", "site?", "site[1]", "{site}", "a:b", "站点",
        ] {
            let overrides = HashMap::from([
                ("cache.driver".into(), "redis".into()),
                ("cache.redis.namespace".into(), namespace.into()),
            ]);
            assert!(Config::load(None, &overrides).is_err(), "{namespace}");
        }
        let overrides = HashMap::from([
            ("cache.driver".into(), "redis".into()),
            ("cache.redis.namespace".into(), "blog.example-prod_1".into()),
        ]);
        let cfg = Config::load(None, &overrides).unwrap();
        assert_eq!(cfg.cache.redis.namespace, "blog.example-prod_1");
        let oversized = RedisCacheConfig {
            namespace: "a".repeat(129),
            ..Default::default()
        };
        assert!(oversized.validate().is_err());
    }

    #[test]
    fn security_config_rejects_placeholders_and_parses_proxy_overrides() {
        for secret in ["CHANGE_ME", " change_me ", "   "] {
            let overrides = HashMap::from([("security.secret".into(), secret.into())]);
            assert!(Config::load(None, &overrides).is_err());
        }
        let overrides = HashMap::from([(
            "security.trusted_proxies".into(),
            "[\"127.0.0.1\", \"::1\"]".into(),
        )]);
        let cfg = Config::load(None, &overrides).unwrap();
        assert_eq!(cfg.security.trusted_proxies.len(), 2);
        let overrides =
            HashMap::from([("security.trusted_proxies".into(), "[\"not-an-ip\"]".into())]);
        assert!(Config::load(None, &overrides).is_err());
    }

    #[test]
    fn overrides_apply() {
        let mut ov = HashMap::new();
        ov.insert("server.port".to_string(), "8080".to_string());
        ov.insert("theme.active".to_string(), "dark".to_string());
        ov.insert("site.title".to_string(), "My Blog".to_string());
        ov.insert("comments.enabled".to_string(), "false".to_string());
        let cfg = Config::load(None, &ov).unwrap();
        assert_eq!(cfg.server.port, 8080);
        assert_eq!(cfg.theme.active, "dark");
        assert_eq!(cfg.site.title, "My Blog");
        assert!(!cfg.comments.enabled);
    }

    #[test]
    fn media_defaults() {
        let cfg = Config::load(None, &HashMap::new()).unwrap();
        assert!(cfg.media.enabled);
        assert_eq!(cfg.media.provider_normalized(), "local");
        assert!(cfg.media.upload.deduplicate);
        assert!(cfg.media.exif.strip);
        assert_eq!(cfg.media.upload.limits.for_kind("image"), 20 * 1024 * 1024);
        assert_eq!(cfg.media.upload.limits.for_kind("video"), 500 * 1024 * 1024);
        // Body limit covers the largest per-kind cap.
        assert_eq!(cfg.media.max_upload_bytes(), 500 * 1024 * 1024);
    }

    #[test]
    fn extension_defaults() {
        let cfg = Config::load(None, &HashMap::new()).unwrap();
        assert_eq!(cfg.extensions.upload.max_file_bytes(), 20 * 1024 * 1024);
        assert_eq!(
            cfg.extensions.upload.max_uncompressed_bytes(),
            100 * 1024 * 1024
        );
        assert_eq!(cfg.extensions.upload.max_files, 5000);
        assert!(!cfg.extensions.security.require_signature);
    }

    #[test]
    fn size_parsing() {
        assert_eq!(parse_size("20MB"), Some(20 * 1024 * 1024));
        assert_eq!(parse_size("500MiB"), Some(500 * 1024 * 1024));
        assert_eq!(
            parse_size("1.5GB"),
            Some((1.5 * 1024.0 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(parse_size("2M"), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("512KB"), Some(512 * 1024));
        assert_eq!(parse_size("100"), Some(100));
        assert_eq!(parse_size(""), None);
        assert_eq!(parse_size("abc"), None);
        assert_eq!(parse_size("10XB"), None);
    }
}
