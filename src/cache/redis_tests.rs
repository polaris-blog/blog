//! Opt-in real-server contracts. Tests own random site prefixes, never the
//! Redis database. Run with POLARIS_TEST_REDIS_URL and `--ignored redis_live`.

use super::*;
use crate::config::RedisCacheConfig;
use crate::utils::cookies::random_token;
use ::redis::AsyncCommands;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;

fn config() -> CacheConfig {
    CacheConfig {
        driver: "redis".into(),
        redis: RedisCacheConfig {
            url: std::env::var("POLARIS_TEST_REDIS_URL")
                .expect("set POLARIS_TEST_REDIS_URL to a dedicated test Redis server"),
            namespace: format!("test-{}", random_token(12)),
            ..Default::default()
        },
        ..Default::default()
    }
}

async fn cleanup(cfg: &CacheConfig) {
    let client = ::redis::Client::open(cfg.redis.url.as_str()).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let mut cursor = 0_u64;
    loop {
        let (next, keys): (u64, Vec<String>) = ::redis::cmd("SCAN")
            .arg(cursor)
            .arg("MATCH")
            .arg(format!("polaris:v2:{{{}}}:*", cfg.redis.namespace))
            .arg("COUNT")
            .arg(100)
            .query_async(&mut conn)
            .await
            .unwrap();
        if !keys.is_empty() {
            let _: usize = conn.del(keys).await.unwrap();
        }
        cursor = next;
        if cursor == 0 {
            break;
        }
    }
}

#[tokio::test]
#[ignore = "requires POLARIS_TEST_REDIS_URL"]
async fn redis_live_shared_generations_isolation_and_restart() {
    let cfg = config();
    let mut other_cfg = cfg.clone();
    other_cfg.redis.namespace.push_str("-other");
    let a = CacheManager::build(&cfg).await;
    let b = CacheManager::build(&cfg).await;
    let other = CacheManager::build(&other_cfg).await;
    assert_eq!(a.driver(), "redis");
    a.set_json(ns::POST, "slug:hello", &1_i64).await;
    assert_eq!(b.get_json::<i64>(ns::POST, "slug:hello").await, Some(1));
    assert!(
        other
            .get_json::<i64>(ns::POST, "slug:hello")
            .await
            .is_none()
    );
    other.set_json(ns::POST, "slug:hello", &99_i64).await;
    a.set_json(ns::MEDIA, "id:1", &42_i64).await;
    b.invalidate(&[ns::POST]).await;
    assert!(a.get_json::<i64>(ns::POST, "slug:hello").await.is_none());
    assert_eq!(a.get_json::<i64>(ns::MEDIA, "id:1").await, Some(42));

    // The slower loader must not populate the generation changed by B.
    let value = a
        .get_or_load(ns::POST, "race", async {
            b.invalidate(&[ns::POST]).await;
            Ok(7_i64)
        })
        .await
        .unwrap();
    assert_eq!(value, 7);
    assert!(a.get_json::<i64>(ns::POST, "race").await.is_none());
    for n in ns::CONTENT.iter().chain(std::iter::once(&ns::PAGECACHE)) {
        a.set_json(n, "content", &1_i64).await;
    }
    b.invalidate_content().await;
    for n in ns::CONTENT.iter().chain(std::iter::once(&ns::PAGECACHE)) {
        assert!(a.get_json::<i64>(n, "content").await.is_none(), "{n}");
    }

    // Clear fences outstanding fills, including arbitrary namespaces that
    // the clearing instance has never used; another site is unaffected.
    a.set_json("custom", "k", &3_i64).await;
    let fill = a.begin_fill("custom", "inflight").await;
    b.clear_all().await.unwrap();
    a.finish_fill(fill, &4_i64).await;
    assert!(a.get_json::<i64>("custom", "k").await.is_none());
    assert!(a.get_json::<i64>("custom", "inflight").await.is_none());
    assert_eq!(
        other.get_json::<i64>(ns::POST, "slug:hello").await,
        Some(99)
    );

    // New processes rotate the epoch, even if Redis retained v0-era data.
    a.set_json(ns::POST, "restart", &5_i64).await;
    let fill = a.begin_fill(ns::POST, "restart-race").await;
    let restarted = CacheManager::build(&cfg).await;
    a.finish_fill(fill, &6_i64).await;
    assert!(
        restarted
            .get_json::<i64>(ns::POST, "restart")
            .await
            .is_none()
    );
    assert!(
        restarted
            .get_json::<i64>(ns::POST, "restart-race")
            .await
            .is_none()
    );
    restarted.set_json(ns::POST, "new", &8_i64).await;
    assert_eq!(b.get_json::<i64>(ns::POST, "new").await, Some(8));
    assert!(a.delete_logical("post:new").await);
    assert!(b.get_json::<i64>(ns::POST, "new").await.is_none());

    // Evict metadata only, leaving live old data: never resurrect it.
    a.set_json(ns::POST, "evicted", &10_i64).await;
    let client = ::redis::Client::open(cfg.redis.url.as_str()).unwrap();
    let mut conn = client.get_multiplexed_async_connection().await.unwrap();
    let _: usize = conn
        .del(format!("polaris:v2:{{{}}}:meta", cfg.redis.namespace))
        .await
        .unwrap();
    assert!(b.get_json::<i64>(ns::POST, "evicted").await.is_none());
    assert_eq!(
        other.get_json::<i64>(ns::POST, "slug:hello").await,
        Some(99)
    );
    cleanup(&cfg).await;
    cleanup(&other_cfg).await;
}

/// Drop only this test's sockets. No server-wide CLIENT KILL, shutdown or
/// pause commands, so failure tests can run alongside the other contracts.
struct FaultProxy {
    client: ::redis::Client,
    online: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl FaultProxy {
    async fn new(url: &str) -> Self {
        let client = ::redis::Client::open(url).unwrap();
        let info = client.get_connection_info().clone();
        let upstream = match info.addr() {
            ::redis::ConnectionAddr::Tcp(host, port) => format!("{host}:{port}"),
            _ => panic!("failure tests require a redis:// TCP URL"),
        };
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = ::redis::Client::open(info.set_addr(::redis::ConnectionAddr::Tcp(
            "127.0.0.1".into(),
            addr.port(),
        )))
        .unwrap();
        let (online, receiver) = watch::channel(true);
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut downstream, _)) = listener.accept().await else {
                    break;
                };
                let upstream = upstream.clone();
                let mut receiver = receiver.clone();
                tokio::spawn(async move {
                    if !*receiver.borrow_and_update() {
                        return;
                    }
                    let Ok(mut upstream) = TcpStream::connect(upstream).await else {
                        return;
                    };
                    tokio::select! {
                        _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream) => {},
                        _ = receiver.changed() => {},
                    }
                });
            }
        });
        Self {
            client,
            online,
            task,
        }
    }

    async fn disconnect(&self) {
        self.online.send(false).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
    }

    fn reconnect(&self) {
        self.online.send(true).unwrap();
    }
}

impl Drop for FaultProxy {
    fn drop(&mut self) {
        let _ = self.online.send(false);
        self.task.abort();
    }
}

// Build through the same implementation with proxy connection info that
// retains the server's username/password/database, without printing secrets.
async fn manager_via_proxy(cfg: &CacheConfig, proxy: &FaultProxy) -> Arc<CacheManager> {
    let backend = redis::RedisCache::connect_with_client(&cfg.redis, proxy.client.clone())
        .await
        .unwrap();
    let mut manager = CacheManager::disabled(cfg, Arc::new(MemoryCache::new(10)));
    manager.enabled = true;
    manager.backend = CacheBackend::Redis(backend);
    Arc::new(manager)
}

#[tokio::test]
#[ignore = "requires POLARIS_TEST_REDIS_URL; tests the real 30-second retry window"]
async fn redis_live_disconnect_bypasses_and_recovers() {
    let cfg = config();
    let peer = CacheManager::build(&cfg).await;
    let proxy = FaultProxy::new(&cfg.redis.url).await;
    let a = manager_via_proxy(&cfg, &proxy).await;
    a.set_json(ns::POST, "old", &1_i64).await;
    assert_eq!(peer.get_json::<i64>(ns::POST, "old").await, Some(1));
    let fill = a.begin_fill(ns::POST, "old-fill").await;
    proxy.disconnect().await;
    a.invalidate_content().await;
    let started = std::time::Instant::now();
    let fresh = a
        .get_or_load(ns::POST, "old", async { Ok(2_i64) })
        .await
        .unwrap();
    assert_eq!(fresh, 2);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "degraded reads must bypass quickly"
    );
    let offline_fill = a.begin_fill(ns::POST, "offline-fill").await;
    assert!(
        a.clear_all().await.is_err(),
        "admin clear must report failure"
    );
    proxy.reconnect();
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert!(a.get_json::<i64>(ns::POST, "old").await.is_none());
    assert!(peer.get_json::<i64>(ns::POST, "old").await.is_none());
    a.finish_fill(fill, &1_i64).await;
    a.finish_fill(offline_fill, &2_i64).await;
    assert!(peer.get_json::<i64>(ns::POST, "old-fill").await.is_none());
    assert!(
        peer.get_json::<i64>(ns::POST, "offline-fill")
            .await
            .is_none()
    );
    a.set_json(ns::POST, "recovered", &3_i64).await;
    assert_eq!(peer.get_json::<i64>(ns::POST, "recovered").await, Some(3));
    cleanup(&cfg).await;
}

#[tokio::test]
#[ignore = "requires POLARIS_TEST_REDIS_URL; tests the real 30-second retry window"]
async fn redis_live_unavailable_at_startup_can_recover() {
    let cfg = config();
    let peer = CacheManager::build(&cfg).await;
    peer.set_json(ns::POST, "before", &1_i64).await;
    let proxy = FaultProxy::new(&cfg.redis.url).await;
    proxy.disconnect().await;
    let a = manager_via_proxy(&cfg, &proxy).await;
    assert_eq!(a.driver(), "redis");
    a.invalidate_content().await;
    assert_eq!(
        a.get_or_load(ns::POST, "before", async { Ok(2_i64) })
            .await
            .unwrap(),
        2
    );
    proxy.reconnect();
    tokio::time::sleep(Duration::from_secs(31)).await;
    assert!(a.get_json::<i64>(ns::POST, "before").await.is_none());
    assert!(peer.get_json::<i64>(ns::POST, "before").await.is_none());
    a.set_json(ns::POST, "after", &3_i64).await;
    assert_eq!(peer.get_json::<i64>(ns::POST, "after").await, Some(3));
    cleanup(&cfg).await;
}
