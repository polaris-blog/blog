//! Optional shared cache (`--features redis`). Each site owns a metadata
//! hash and TTL-bound data keys under `polaris:v2:{site}:*`.
//!
//! Reads and fills consult shared generations; invalidation changes them
//! atomically. Random generations avoid reusing old data if metadata is
//! evicted. Startup/reconnection rotates the site's epoch before any data
//! access, fencing fills and mutations made while this client was offline.
//! Failed operations bypass caching for 30 seconds, then retry on demand.

use std::time::{Duration, Instant};

use redis::AsyncCommands;
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use tokio::sync::Mutex;

use super::Cache;
use crate::config::RedisCacheConfig;
use crate::utils::cookies::random_token;

const DEGRADE_FOR: Duration = Duration::from_secs(30);

// One atomic snapshot, including initialization after metadata eviction.
const GENERATION: &str = r#"
local epoch = redis.call('HGET', KEYS[1], 'epoch')
if not epoch then
    epoch = ARGV[2]
    redis.call('HSET', KEYS[1], 'epoch', epoch)
end
local generation = redis.call('HGET', KEYS[1], ARGV[1])
if not generation then
    generation = ARGV[3]
    redis.call('HSET', KEYS[1], ARGV[1], generation)
end
return epoch .. '.' .. generation
"#;

struct ConnectionState {
    conn: Option<ConnectionManager>,
    retry_at: Option<Instant>,
}

pub struct RedisCache {
    client: redis::Client,
    prefix: String,
    state: Mutex<ConnectionState>,
}

impl RedisCache {
    /// Invalid configuration is an error. An unreachable server yields a
    /// degraded backend that can recover; never a private memory fallback.
    pub async fn connect(cfg: &RedisCacheConfig) -> anyhow::Result<Self> {
        Self::connect_with_client(cfg, redis::Client::open(cfg.url.as_str())?).await
    }

    pub(super) async fn connect_with_client(
        cfg: &RedisCacheConfig,
        client: redis::Client,
    ) -> anyhow::Result<Self> {
        cfg.validate()?;
        let cache = Self {
            client,
            prefix: format!("polaris:v2:{{{}}}:", cfg.namespace),
            state: Mutex::new(ConnectionState {
                conn: None,
                retry_at: None,
            }),
        };
        let _ = cache.connection().await;
        Ok(cache)
    }

    fn metadata_key(&self) -> String {
        format!("{}meta", self.prefix)
    }

    fn key(&self, key: &str) -> String {
        format!("{}data:{key}", self.prefix)
    }

    fn mark_degraded(state: &mut ConnectionState) {
        if state.retry_at.is_none_or(|at| Instant::now() >= at) {
            tracing::warn!(
                retry_secs = DEGRADE_FOR.as_secs(),
                "redis unavailable — bypassing cache until reconnection"
            );
        }
        state.conn = None;
        state.retry_at = Some(Instant::now() + DEGRADE_FOR);
    }

    async fn connection(&self) -> anyhow::Result<ConnectionManager> {
        let mut state = self.state.lock().await;
        if state.retry_at.is_some_and(|at| Instant::now() < at) {
            anyhow::bail!("redis degraded");
        }
        if let Some(conn) = &state.conn {
            return Ok(conn.clone());
        }
        // Serialize recovery: no operation can use this connection before
        // the epoch rotates. The bound covers connection + the initial HSET.
        let connected = tokio::time::timeout(Duration::from_secs(3), async {
            let config = ConnectionManagerConfig::new()
                .set_number_of_retries(0)
                .set_connection_timeout(Some(Duration::from_secs(2)))
                .set_response_timeout(Some(Duration::from_millis(500)));
            let mut conn = ConnectionManager::new_with_config(self.client.clone(), config).await?;
            let _: usize = conn
                .hset(self.metadata_key(), "epoch", random_token(16))
                .await?;
            Ok::<_, redis::RedisError>(conn)
        })
        .await;
        match connected {
            Ok(Ok(conn)) => {
                state.conn = Some(conn.clone());
                state.retry_at = None;
                Ok(conn)
            }
            _ => {
                Self::mark_degraded(&mut state);
                anyhow::bail!("redis connection or generation initialization failed")
            }
        }
    }

    async fn result<T>(&self, result: redis::RedisResult<T>) -> anyhow::Result<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                Self::mark_degraded(&mut *self.state.lock().await);
                // Do not log connection URLs (they may contain credentials).
                tracing::debug!(error_kind = ?error.kind(), "redis command failed");
                anyhow::bail!("redis command failed")
            }
        }
    }

    pub(super) async fn generation(&self, ns: &str) -> anyhow::Result<String> {
        let mut conn = self.connection().await?;
        self.result(
            redis::cmd("EVAL")
                .arg(GENERATION)
                .arg(1)
                .arg(self.metadata_key())
                .arg(format!("ns:{ns}"))
                .arg(random_token(16))
                .arg(random_token(16))
                .query_async(&mut conn)
                .await,
        )
        .await
    }

    /// One HSET changes every requested namespace atomically. Readers ask
    /// Redis for each generation; no pub/sub delivery or local polling lag.
    pub(super) async fn invalidate(&self, namespaces: &[&str]) -> anyhow::Result<()> {
        if namespaces.is_empty() {
            return Ok(());
        }
        let mut conn = self.connection().await?;
        let mut cmd = redis::cmd("HSET");
        cmd.arg(self.metadata_key());
        for ns in namespaces {
            cmd.arg(format!("ns:{ns}")).arg(random_token(16));
        }
        self.result(cmd.query_async::<usize>(&mut conn).await)
            .await?;
        Ok(())
    }
}

impl Cache for RedisCache {
    async fn get(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let mut conn = self.connection().await?;
        self.result(conn.get(self.key(key)).await).await
    }

    async fn set(&self, key: &str, value: Vec<u8>, ttl: Duration) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let ttl = ttl.as_secs().clamp(1, 86_400 * 365);
        self.result(conn.set_ex(self.key(key), value, ttl).await)
            .await
    }

    async fn delete(&self, key: &str) -> anyhow::Result<bool> {
        let mut conn = self.connection().await?;
        let removed: usize = self.result(conn.del(self.key(key)).await).await?;
        Ok(removed > 0)
    }

    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        let mut conn = self.connection().await?;
        self.result(conn.exists(self.key(key)).await).await
    }

    async fn clear(&self) -> anyhow::Result<()> {
        let mut conn = self.connection().await?;
        let _: usize = self
            .result(
                conn.hset(self.metadata_key(), "epoch", random_token(16))
                    .await,
            )
            .await?;
        // Logical clear: old keys age out via TTL. Never delete metadata or
        // scan/delete another site's data; in-flight fills remain obsolete.
        Ok(())
    }

    fn size(&self) -> (usize, Option<usize>) {
        (0, None)
    }
}
