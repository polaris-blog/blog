//! Layered cache subsystem.
//!
//! ```text
//!                HTTP cache (ETag / 304 / Cache-Control)
//!                          ↓
//!                Response cache  (ns "pagecache": rendered HTML)
//!                          ↓
//!                Object cache    (ns "post"/"posts"/"page"/"category"/
//!                                  "tag"/"rss"/"atom"/"sitemap")
//!                          ↓
//!              ┌─────────────┬──────────────┐
//!              │ Memory (dfl)│ Redis (opt.) │     ← `Cache` trait
//!              └─────────────┴──────────────┘
//!                          ↓
//!                Database (SQLite / MySQL / PostgreSQL)
//! ```
//!
//! Design rules:
//! - **Cache failure ≠ application failure.** Backend errors are swallowed
//!   (logged, counted as a miss) and every read falls through to the
//!   database.
//! - **Database is the source of truth** — strict cache-aside: reads fill
//!   the cache, writes invalidate it.
//! - **Invalidation is explicit**, not TTL-based: each namespace carries a
//!   generation baked into the physical key
//!   (`{ns}:v{version}:{sub}`), so bumping a version invalidates the whole
//!   namespace in O(1) on every backend (no SCAN, no key listing).
//! - **Stampede protection**: hot keys are loaded once via single-flight
//!   (see [`coalesce::Coalescer`]).
//!
//! Plugins get their own sandboxed, always-memory cache namespaced
//! `plugin:{name}:*` (sync API — scripts never touch the network).

pub mod coalesce;
pub mod memory;
#[cfg(feature = "redis")]
pub mod redis;
#[cfg(all(test, feature = "redis"))]
mod redis_tests;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::CacheConfig;
use coalesce::Coalescer;
use memory::MemoryCache;

/// Logical namespaces (first segment of every cache key).
pub mod ns {
    /// A single post by id or slug.
    pub const POST: &str = "post";
    /// Post lists (the homepage is `posts` page 1) and archive queries.
    pub const POSTS: &str = "posts";
    /// Page objects and the navigation page list.
    pub const PAGE: &str = "page";
    pub const CATEGORY: &str = "category";
    pub const TAG: &str = "tag";
    pub const RSS: &str = "rss";
    pub const ATOM: &str = "atom";
    pub const SITEMAP: &str = "sitemap";
    /// Search results and suggestions (version-invalidated with content).
    pub const SEARCH: &str = "search";
    /// Media metadata by id (never the binary content — that lives in
    /// storage/CDN and is served with immutable hash URLs).
    pub const MEDIA: &str = "media";
    /// Rendered HTML responses (keyed by host + path).
    pub const PAGECACHE: &str = "pagecache";

    /// Content namespaces invalidated together on any content mutation
    /// (post/page/term/user-profile/settings changes).
    pub const CONTENT: &[&str] = &[POST, POSTS, PAGE, CATEGORY, TAG, RSS, ATOM, SITEMAP, SEARCH];
}

/// Abstract cache backend contract. Implement this trait to add a new
/// backend, then extend [`CacheBackend`].
pub trait Cache: Send + Sync + 'static {
    fn get(&self, key: &str) -> impl Future<Output = anyhow::Result<Option<Vec<u8>>>> + Send;
    fn set(
        &self,
        key: &str,
        value: Vec<u8>,
        ttl: Duration,
    ) -> impl Future<Output = anyhow::Result<()>> + Send;
    fn delete(&self, key: &str) -> impl Future<Output = anyhow::Result<bool>> + Send;
    fn exists(&self, key: &str) -> impl Future<Output = anyhow::Result<bool>> + Send;
    fn clear(&self) -> impl Future<Output = anyhow::Result<()>> + Send;
    /// `(live entries, approximate bytes)` — `None` bytes when unknown.
    fn size(&self) -> (usize, Option<usize>);
}

/// Runtime-selected backend. Enum dispatch keeps calls monomorphic —
/// no `Box<dyn Future>` per operation, no `async-trait` dependency.
pub enum CacheBackend {
    Disabled,
    Memory(MemoryCache),
    #[cfg(feature = "redis")]
    Redis(redis::RedisCache),
}

impl CacheBackend {
    /// Errors are mapped to `None`: a failing cache must look like a miss.
    pub async fn get(&self, key: &str) -> Option<Vec<u8>> {
        let res = match self {
            Self::Disabled => return None,
            Self::Memory(m) => m.get(key).await,
            #[cfg(feature = "redis")]
            Self::Redis(r) => r.get(key).await,
        };
        res.ok().flatten()
    }

    /// Errors are swallowed (logged at debug): a failing write must not
    /// fail the request.
    pub async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration) {
        let res = match self {
            Self::Disabled => return,
            Self::Memory(m) => m.set(key, value, ttl).await,
            #[cfg(feature = "redis")]
            Self::Redis(r) => r.set(key, value, ttl).await,
        };
        if let Err(e) = res {
            tracing::debug!(error = %e, key, "cache set failed (ignored)");
        }
    }

    /// Returns true when the key existed and was removed.
    pub async fn delete(&self, key: &str) -> bool {
        let res = match self {
            Self::Disabled => return false,
            Self::Memory(m) => m.delete(key).await,
            #[cfg(feature = "redis")]
            Self::Redis(r) => r.delete(key).await,
        };
        res.unwrap_or(false)
    }

    /// Surface errors — used by the admin "clear cache" action.
    pub async fn clear(&self) -> anyhow::Result<()> {
        match self {
            Self::Disabled => Ok(()),
            Self::Memory(m) => m.clear().await,
            #[cfg(feature = "redis")]
            Self::Redis(r) => r.clear().await,
        }
    }

    fn size(&self) -> (usize, Option<usize>) {
        match self {
            Self::Disabled => (0, None),
            Self::Memory(m) => m.size(),
            #[cfg(feature = "redis")]
            Self::Redis(r) => r.size(),
        }
    }

    fn evictions(&self) -> u64 {
        match self {
            Self::Memory(m) => m.evictions_sync(),
            _ => 0,
        }
    }

    pub fn driver(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Memory(_) => "memory",
            #[cfg(feature = "redis")]
            Self::Redis(_) => "redis",
        }
    }
}

/// Point-in-time statistics snapshot (admin API / dashboard).
#[derive(Clone, Debug, Serialize)]
pub struct CacheStatsSnapshot {
    pub enabled: bool,
    pub driver: String,
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
    pub entries: usize,
    pub memory_bytes: Option<usize>,
    pub hit_rate: f64,
}

/// Cache facade: namespaces, TTL policy, invalidation, single-flight.
pub struct CacheManager {
    enabled: bool,
    backend: CacheBackend,
    ttl: crate::config::CacheTtlConfig,
    default_ttl: u64,
    /// Version per namespace; baked into physical keys for O(1)
    /// invalidation of a whole namespace.
    versions: RwLock<HashMap<String, u64>>,
    coalescer: Coalescer,
    /// Sandboxed sync cache for plugins (`plugin:{name}:*`).
    plugin_cache: Arc<MemoryCache>,
    hits: AtomicU64,
    misses: AtomicU64,
}

/// A fill must target the generation observed before loading its source.
pub(crate) struct CacheFill {
    key: Option<String>,
    ttl: Duration,
}

/// Plugin cache capacity (entries). Small by design: plugin data is a
/// convenience, never authoritative.
const PLUGIN_CACHE_ENTRIES: usize = 1024;

impl CacheManager {
    /// Build from configuration. Redis outages bypass caching and retry;
    /// invalid Redis configuration disables caching rather than isolating
    /// this instance behind an incoherent private memory fallback.
    pub async fn build(cfg: &CacheConfig) -> Arc<Self> {
        let plugin_cache = Arc::new(MemoryCache::new(PLUGIN_CACHE_ENTRIES));
        if !cfg.enabled {
            tracing::info!("cache disabled by configuration");
            return Arc::new(Self::disabled(cfg, plugin_cache));
        }

        let backend = match cfg.driver.to_ascii_lowercase().as_str() {
            #[cfg(feature = "redis")]
            "redis" => match redis::RedisCache::connect(&cfg.redis).await {
                Ok(r) => {
                    tracing::info!("cache backend: redis");
                    CacheBackend::Redis(r)
                }
                Err(_) => {
                    tracing::warn!("invalid Redis configuration — cache disabled");
                    return Arc::new(Self::disabled(cfg, plugin_cache));
                }
            },
            #[cfg(not(feature = "redis"))]
            "redis" => {
                tracing::warn!(
                    "configured driver 'redis' but this build has no redis support \
                     (rebuild with --features redis) — cache disabled"
                );
                return Arc::new(Self::disabled(cfg, plugin_cache));
            }
            "memory" | "" => {
                tracing::info!(
                    max_entries = cfg.memory.max_entries,
                    "cache backend: memory"
                );
                CacheBackend::Memory(MemoryCache::new(cfg.memory.max_entries))
            }
            other => {
                tracing::warn!(driver = other, "unknown cache driver — using memory");
                CacheBackend::Memory(MemoryCache::new(cfg.memory.max_entries))
            }
        };

        Arc::new(Self {
            enabled: true,
            backend,
            ttl: cfg.ttl.clone(),
            default_ttl: cfg.default_ttl,
            versions: RwLock::new(HashMap::new()),
            coalescer: Coalescer::new(),
            plugin_cache,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        })
    }

    fn disabled(cfg: &CacheConfig, plugin_cache: Arc<MemoryCache>) -> Self {
        Self {
            enabled: false,
            backend: CacheBackend::Disabled,
            ttl: cfg.ttl.clone(),
            default_ttl: cfg.default_ttl,
            versions: RwLock::new(HashMap::new()),
            coalescer: Coalescer::new(),
            plugin_cache,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn driver(&self) -> &'static str {
        if !self.enabled {
            return "disabled";
        }
        self.backend.driver()
    }

    // -- keys & versions ------------------------------------------------------

    fn version_of(&self, ns: &str) -> u64 {
        if let Some(version) = crate::utils::lock::read(&self.versions).get(ns) {
            return *version;
        }
        *crate::utils::lock::write(&self.versions)
            .entry(ns.to_string())
            .or_insert(0)
    }

    async fn phys_key(&self, ns: &str, sub: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        #[cfg(feature = "redis")]
        if let CacheBackend::Redis(redis) = &self.backend {
            let generation = redis.generation(ns).await.ok()?;
            return Some(format!("{ns}:v{generation}:{sub}"));
        }
        Some(format!("{ns}:v{}:{sub}", self.version_of(ns)))
    }

    /// Invalidate whole namespaces (O(1), no scan): the version bump makes
    /// every current key unreachable; stale entries age out via TTL/LRU.
    pub async fn invalidate(&self, namespaces: &[&str]) {
        #[cfg(feature = "redis")]
        if let CacheBackend::Redis(redis) = &self.backend {
            // Failure has already gated this backend. Recovery rotates the
            // entire site's epoch, covering invalidations missed offline.
            let _ = redis.invalidate(namespaces).await;
            return;
        }
        let mut v = crate::utils::lock::write(&self.versions);
        for ns in namespaces {
            *v.entry((*ns).to_string()).or_insert(0) += 1;
        }
    }

    /// Full content invalidation — called on every content mutation
    /// (post/page/term/settings changes).
    pub async fn invalidate_content(&self) {
        let mut namespaces = ns::CONTENT.to_vec();
        namespaces.push(ns::PAGECACHE);
        self.invalidate(&namespaces).await;
    }

    /// Invalidate rendered HTML only (e.g. comment moderation, which does
    /// not change post objects or feeds).
    pub async fn invalidate_pages(&self) {
        self.invalidate(&[ns::PAGECACHE]).await;
    }

    // -- TTL policy -----------------------------------------------------------

    pub fn ttl_for(&self, ns: &str) -> Duration {
        let secs = match ns {
            ns::POST => self.ttl.post,
            ns::POSTS => self.ttl.posts,
            ns::PAGE => self.ttl.page,
            ns::CATEGORY => self.ttl.category,
            ns::TAG => self.ttl.tag,
            ns::RSS => self.ttl.rss,
            ns::ATOM => self.ttl.atom,
            ns::SITEMAP => self.ttl.sitemap,
            ns::SEARCH => self.ttl.search,
            // Rendered HTML rides the homepage/list TTL (most volatile
            // public surface; 60s by default).
            ns::PAGECACHE => self.ttl.homepage,
            _ => self.default_ttl,
        };
        Duration::from_secs(secs.max(1))
    }

    // -- raw reads/writes -----------------------------------------------------

    async fn raw_get(&self, ns: &str, sub: &str) -> Option<Vec<u8>> {
        if !self.enabled {
            return None;
        }
        let out = match self.phys_key(ns, sub).await {
            Some(key) => self.backend.get(&key).await,
            None => None,
        };
        if out.is_none() {
            self.misses.fetch_add(1, Ordering::Relaxed);
        }
        out
    }

    async fn raw_set(&self, ns: &str, sub: &str, bytes: Vec<u8>) {
        if !self.enabled {
            return;
        }
        if let Some(key) = self.phys_key(ns, sub).await {
            self.backend.set(&key, bytes, self.ttl_for(ns)).await;
        }
    }

    pub async fn get_bytes(&self, ns: &str, sub: &str) -> Option<Vec<u8>> {
        let out = self.raw_get(ns, sub).await;
        if out.is_some() {
            self.hits.fetch_add(1, Ordering::Relaxed);
        }
        out
    }

    pub async fn set_bytes(&self, ns: &str, sub: &str, bytes: Vec<u8>) {
        self.raw_set(ns, sub, bytes).await;
    }

    pub async fn get_json<T: DeserializeOwned>(&self, ns: &str, sub: &str) -> Option<T> {
        let raw = self.raw_get(ns, sub).await?;
        match serde_json::from_slice(&raw) {
            Ok(v) => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                Some(v)
            }
            Err(_) => {
                // Corrupt entry: drop it and count a miss.
                self.misses.fetch_add(1, Ordering::Relaxed);
                self.delete(ns, sub).await;
                None
            }
        }
    }

    pub async fn set_json<T: Serialize>(&self, ns: &str, sub: &str, value: &T) {
        if let Ok(bytes) = serde_json::to_vec(value) {
            self.raw_set(ns, sub, bytes).await;
        }
    }

    pub(crate) async fn begin_fill(&self, ns: &str, sub: &str) -> CacheFill {
        CacheFill {
            key: self.phys_key(ns, sub).await,
            ttl: self.ttl_for(ns),
        }
    }

    pub(crate) async fn finish_fill<T: Serialize>(&self, fill: CacheFill, value: &T) {
        if self.enabled
            && let Some(key) = fill.key
            && let Ok(bytes) = serde_json::to_vec(value)
        {
            self.backend.set(&key, bytes, fill.ttl).await;
        }
    }

    pub async fn delete(&self, ns: &str, sub: &str) {
        if !self.enabled {
            return;
        }
        if let Some(key) = self.phys_key(ns, sub).await {
            self.backend.delete(&key).await;
        }
    }

    /// Delete by logical key (admin API). Accepts `"post:slug:hello"` or a
    /// full physical key `"post:v3:slug:hello"`. Returns true if something
    /// was deleted.
    pub async fn delete_logical(&self, logical: &str) -> bool {
        let trimmed = logical.trim().trim_start_matches("polaris:");
        // Try the exact key first (physical form).
        if self.enabled && self.backend.delete(trimmed).await {
            return true;
        }
        // Reconstruct `ns:v{ver}:rest` from a logical key.
        if let Some((ns, rest)) = trimmed.split_once(':')
            && self.enabled
            && let Some(key) = self.phys_key(ns, rest).await
        {
            return self.backend.delete(&key).await;
        }
        false
    }

    // -- cache-aside with single-flight ----------------------------------------

    /// Cache-aside read: hit → deserialize; miss → load under a per-key
    /// single-flight lock (concurrent misses collapse into one loader),
    /// fill the cache, return. Loader errors propagate untouched.
    pub async fn get_or_load<T, F>(&self, ns: &str, sub: &str, load: F) -> anyhow::Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: std::future::Future<Output = anyhow::Result<T>>,
    {
        if !self.enabled {
            return load.await;
        }
        if let Some(v) = self.get_json::<T>(ns, sub).await {
            return Ok(v);
        }
        let ns = ns.to_string();
        let sub = sub.to_string();
        self.coalescer
            .run(format!("{ns}:{sub}"), async {
                // Double-check: the leader may have filled the entry while
                // this request waited on the lock.
                if let Some(v) = self.get_json::<T>(&ns, &sub).await {
                    return Ok(v);
                }
                let fill = self.begin_fill(&ns, &sub).await;
                let v = load.await?;
                self.finish_fill(fill, &v).await;
                Ok(v)
            })
            .await
    }

    /// Run an arbitrary future under single-flight (used by the response
    /// cache middleware, whose "value" is a live `Response`).
    pub async fn coalesce<F>(&self, key: String, fut: F) -> F::Output
    where
        F: std::future::Future,
    {
        self.coalescer.run(key, fut).await
    }

    // -- admin -----------------------------------------------------------------

    /// Advance generations before clearing so in-flight fills stay obsolete.
    pub async fn clear_all(&self) -> anyhow::Result<()> {
        for version in crate::utils::lock::write(&self.versions).values_mut() {
            *version += 1;
        }
        self.backend.clear().await?;
        self.plugin_cache.clear_sync();
        Ok(())
    }

    pub fn stats(&self) -> CacheStatsSnapshot {
        let (entries, bytes) = self.backend.size();
        let (p_entries, p_bytes) = self.plugin_cache.size();
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        let total = hits + misses;
        CacheStatsSnapshot {
            enabled: self.enabled,
            driver: self.driver().to_string(),
            hits,
            misses,
            evictions: self.backend.evictions(),
            entries: entries + p_entries,
            memory_bytes: bytes.map(|b| b + p_bytes.unwrap_or(0)),
            hit_rate: if total == 0 {
                0.0
            } else {
                hits as f64 / total as f64
            },
        }
    }

    // -- plugin API (sync, sandboxed, namespaced) -------------------------------

    pub fn plugin_cache_handle(&self) -> Arc<MemoryCache> {
        self.plugin_cache.clone()
    }

    pub fn plugin_get(&self, plugin: &str, key: &str) -> Option<String> {
        self.plugin_cache
            .get_sync(&format!("plugin:{plugin}:{key}"))
            .map(|b| String::from_utf8_lossy(&b).into_owned())
    }

    pub fn plugin_set(&self, plugin: &str, key: &str, value: &str, ttl_secs: i64) {
        self.plugin_cache.set_sync(
            &format!("plugin:{plugin}:{key}"),
            Arc::new(value.as_bytes().to_vec()),
            Duration::from_secs(ttl_secs.clamp(1, 86_400 * 365) as u64),
        );
    }

    pub fn plugin_delete(&self, plugin: &str, key: &str) {
        self.plugin_cache
            .del_sync(&format!("plugin:{plugin}:{key}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::CacheConfig;

    #[tokio::test]
    async fn invalidation_during_load_does_not_repopulate_current_generation() {
        let m = mgr().await;
        let value = m
            .get_or_load(ns::POST, "race", async {
                m.invalidate(&[ns::POST]).await;
                Ok(7_i64)
            })
            .await
            .unwrap();
        assert_eq!(value, 7);
        assert!(m.get_json::<i64>(ns::POST, "race").await.is_none());
        let fill = m.begin_fill(ns::POST, "clear-race").await;
        m.clear_all().await.unwrap();
        m.finish_fill(fill, &9_i64).await;
        assert!(m.get_json::<i64>(ns::POST, "clear-race").await.is_none());
    }

    async fn mgr() -> Arc<CacheManager> {
        CacheManager::build(&CacheConfig::default()).await
    }

    #[tokio::test]
    async fn namespace_version_invalidates() {
        let m = mgr().await;
        m.set_json(ns::POST, "slug:hello", &"v1").await;
        assert_eq!(
            m.get_json::<String>(ns::POST, "slug:hello")
                .await
                .as_deref(),
            Some("v1")
        );
        m.invalidate(&[ns::POST]).await;
        assert!(m.get_json::<String>(ns::POST, "slug:hello").await.is_none());
    }

    #[tokio::test]
    async fn invalidate_content_covers_all_namespaces() {
        let m = mgr().await;
        for n in ns::CONTENT.iter().chain(std::iter::once(&ns::PAGECACHE)) {
            m.set_json(n, "x", &1i64).await;
        }
        m.invalidate_content().await;
        for n in ns::CONTENT.iter().chain(std::iter::once(&ns::PAGECACHE)) {
            assert!(
                m.get_json::<i64>(n, "x").await.is_none(),
                "{n} not invalidated"
            );
        }
    }

    #[tokio::test]
    async fn stats_count_hits_and_misses() {
        let m = mgr().await;
        let _: Option<String> = m.get_json(ns::POST, "k").await; // miss
        m.set_json(ns::POST, "k", &"x").await;
        let _: Option<String> = m.get_json(ns::POST, "k").await; // hit
        let s = m.stats();
        assert_eq!(s.hits, 1);
        assert_eq!(s.misses, 1);
        assert!((s.hit_rate - 0.5).abs() < 1e-9);
        assert!(s.entries >= 1);
        assert_eq!(s.driver, "memory");
    }

    #[tokio::test]
    async fn get_or_load_caches_and_invalidates() {
        let m = mgr().await;
        let calls = Arc::new(AtomicU64::new(0));
        let c = calls.clone();
        let v: i64 = m
            .get_or_load(ns::RSS, "base", async {
                c.fetch_add(1, Ordering::Relaxed);
                Ok::<_, anyhow::Error>(7)
            })
            .await
            .unwrap();
        assert_eq!(v, 7);
        // Second read hits the cache: loader not called again.
        let v: i64 = m
            .get_or_load(ns::RSS, "base", async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<_, anyhow::Error>(7)
            })
            .await
            .unwrap();
        assert_eq!(v, 7);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        m.invalidate(&[ns::RSS]).await;
        let v: i64 = m
            .get_or_load(ns::RSS, "base", async {
                calls.fetch_add(1, Ordering::Relaxed);
                Ok::<_, anyhow::Error>(8)
            })
            .await
            .unwrap();
        assert_eq!(v, 8);
        assert_eq!(calls.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn plugin_cache_is_namespaced() {
        let m = mgr().await;
        m.plugin_set("alpha", "greet", "hi", 60);
        assert_eq!(m.plugin_get("alpha", "greet").as_deref(), Some("hi"));
        // Another plugin cannot see or hit the same key.
        assert!(m.plugin_get("beta", "greet").is_none());
        m.plugin_delete("alpha", "greet");
        assert!(m.plugin_get("alpha", "greet").is_none());
    }

    #[tokio::test]
    async fn clear_all_resets_everything() {
        let m = mgr().await;
        m.set_json(ns::POST, "a", &1i64).await;
        m.plugin_set("p", "k", "v", 60);
        m.clear_all().await.unwrap();
        assert!(m.get_json::<i64>(ns::POST, "a").await.is_none());
        assert!(m.plugin_get("p", "k").is_none());
    }

    #[tokio::test]
    async fn corrupt_entry_counts_as_miss_and_is_dropped() {
        let m = mgr().await;
        m.set_bytes(ns::POST, "bad", b"not json".to_vec()).await;
        let v = m.get_json::<String>(ns::POST, "bad").await;
        assert!(v.is_none());
        // Entry was deleted, not left behind.
        let raw = m.raw_get(ns::POST, "bad").await;
        assert!(raw.is_none());
    }

    #[tokio::test]
    async fn delete_logical_reconstructs_versioned_key() {
        let m = mgr().await;
        m.set_json(ns::POST, "slug:hello", &"x").await;
        assert!(m.delete_logical("post:slug:hello").await);
        assert!(m.get_json::<String>(ns::POST, "slug:hello").await.is_none());
    }

    #[tokio::test]
    async fn disabled_manager_passes_through() {
        let cfg = CacheConfig {
            enabled: false,
            ..Default::default()
        };
        let m = CacheManager::build(&cfg).await;
        let v: i64 = m
            .get_or_load(ns::POST, "k", async { Ok::<_, anyhow::Error>(3) })
            .await
            .unwrap();
        assert_eq!(v, 3);
        assert!(!m.enabled());
        assert_eq!(m.stats().driver, "disabled");
    }
}
