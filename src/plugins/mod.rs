//! Plugin system powered by [Rhai](https://rhai.rs).
//!
//! Why Rhai (technology evaluation):
//! - **WASM (wasmtime)**: ~30 MB of extra binary, slow instantiation — too heavy.
//! - **Lua (mlua)**: vendored C code, FFI surface — heavier build, weaker sandboxing.
//! - **JavaScript (boa/quickjs)**: large runtime or C dependency.
//! - **External process + IPC**: deployment complexity, violates single-binary goal.
//! - **Rhai**: pure Rust, `Send + Sync`, no unsafe, sandboxed by construction
//!   (no filesystem / network / OS access unless explicitly registered),
//!   in-process call overhead in the microsecond range. Best fit for a
//!   "fast & light" blog engine.
//!
//! A plugin is a directory under `plugins/` containing:
//! ```text
//! plugins/example/
//! ├── plugin.toml          # metadata + route/filter registration
//! ├── main.rhai            # script (entry point)
//! ├── config.schema.toml   # optional configuration schema (admin UI)
//! └── config.toml          # optional file-level defaults
//! ```
//!
//! Configuration: plugins declare their settings in `config.schema.toml`;
//! Polaris validates input, persists values under the `plugin.<name>.*`
//! namespace and exposes them to the script via `config_get*()` host
//! functions and the `init(config)` hook. Scripts have no write access to
//! any configuration namespace.
//!
//! Capabilities: by default a plugin has no I/O at all. Declaring
//! `permissions = ["network.fetch"]` in `plugin.toml` grants the generic,
//! SSRF-hardened `http_*` API (see `http.rs`); JSON helpers (`json_parse`,
//! `json_stringify`), crypto/encoding helpers (`sha256_hex`,
//! `hmac_sha256_hex`, `base64_*`, `url_encode`) and timestamps (`now`,
//! `now_iso`) are always available.
//!
//! ABI stability: plugins are *source scripts*, not compiled artifacts, so
//! there is no native ABI to break. The hook surface is documented in
//! `docs/DEVELOPMENT.md`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use rhai::{AST, Dynamic, Engine, EvalAltResult, Scope};
use serde::Deserialize;

use crate::cache::memory::MemoryCache;
use crate::config_store::{ConfigManager, plugin_ns};

pub mod crypto;
pub mod http;
pub mod jobs;
pub mod stats;

// ---------------------------------------------------------------------------
// Plugin metadata & loading
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize)]
pub struct PluginMeta {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
    #[serde(default = "default_entry")]
    pub entry: String,
}

fn default_entry() -> String {
    "main.rhai".into()
}

#[derive(Deserialize, Default)]
struct PluginToml {
    #[serde(flatten)]
    meta: PluginMeta,
    #[serde(default)]
    permissions: Vec<String>,
    #[serde(default)]
    routes: HashMap<String, String>,
    #[serde(default)]
    admin_routes: HashMap<String, String>,
    #[serde(default)]
    filters: HashMap<String, String>,
    #[serde(default)]
    jobs: HashMap<String, String>,
}

pub struct Plugin {
    pub meta: PluginMeta,
    /// Declared permissions (manifest), consulted by capability dispatchers
    /// such as the request-guard hook.
    pub permissions: Vec<String>,
    /// Per-plugin engine: host functions that carry plugin identity (the
    /// cache API) bake the plugin's namespace in at build time.
    pub engine: Arc<Engine>,
    pub ast: AST,
    pub fns: HashSet<String>,
    pub routes: HashMap<String, String>,
    pub admin_routes: HashMap<String, String>,
    /// filter_name -> fn_name (Tera template filters).
    pub filters: HashMap<String, String>,
    pub config: rhai::Map,
    pub jobs: Arc<RwLock<HashMap<String, String>>>,
}

#[derive(Default)]
struct Inner {
    enabled: HashMap<String, Plugin>,
    /// filter_name -> (plugin_name, fn_name)
    filters: HashMap<String, (String, String)>,
    generation: u64,
}

pub struct PluginManager {
    dir: PathBuf,
    /// Sandboxed plugin cache (always in-memory, sync — scripts never
    /// perform network I/O). `None` disables the script cache API.
    plugin_cache: Option<Arc<MemoryCache>>,
    /// Schema-backed configuration store (read-only from scripts).
    configs: Arc<ConfigManager>,
    /// Instance secret — the source for per-plugin signing keys (see
    /// crypto::register_signing). Empty in tests unless provided.
    instance_secret: String,
    /// In-memory counters exposed as stat_incr/stat_get (see stats.rs).
    stats: std::sync::Arc<crate::plugins::stats::Stats>,
    inner: RwLock<Inner>,
    scheduler: Option<std::sync::Weak<crate::scheduler::Scheduler>>,
}

pub struct RouteResult {
    pub status: u16,
    pub content_type: String,
    pub body: String,
}

/// Permission required before a plugin's `request_guard` hook is invoked.
pub const REQUEST_GUARD_PERMISSION: &str = "request.guard";

/// Verdict of the `request_guard` hook chain over one request.
#[derive(Debug)]
pub enum GuardOutcome {
    /// No plugin objected — the request proceeds.
    Allow,
    /// 303 redirect (gate pages, maintenance notices, …).
    Redirect(String),
    /// A direct response (403 blocks, custom interstitials, …).
    Respond { status: u16, body: String },
}

impl PluginManager {
    pub fn new(
        dir: &Path,
        enabled: &[String],
        plugin_cache: Option<Arc<MemoryCache>>,
        configs: Arc<ConfigManager>,
    ) -> Self {
        Self::with_scheduler(dir, enabled, plugin_cache, configs, None).with_secret("")
    }

    pub fn with_scheduler(
        dir: &Path,
        enabled: &[String],
        plugin_cache: Option<Arc<MemoryCache>>,
        configs: Arc<ConfigManager>,
        scheduler: Option<std::sync::Weak<crate::scheduler::Scheduler>>,
    ) -> Self {
        let mgr = Self {
            dir: dir.to_path_buf(),
            plugin_cache,
            configs,
            instance_secret: String::new(),
            stats: std::sync::Arc::new(crate::plugins::stats::Stats::new()),
            inner: RwLock::new(Inner::default()),
            scheduler,
        };
        mgr.reload(enabled);
        mgr
    }

    /// Provide the instance secret as the root of per-plugin signing keys
    /// (`sign_hex`). Builder-style: `PluginManager::with_scheduler(…)
    /// .with_secret(&cfg.security.secret)`.
    pub fn with_secret(mut self, secret: &str) -> Self {
        self.instance_secret = secret.to_string();
        self
    }

    pub fn reload(&self, enabled: &[String]) {
        let mut new_inner = Inner {
            generation: self.generation() + 1,
            ..Default::default()
        };
        for name in enabled {
            match self.load_plugin(name) {
                Ok(plugin) => {
                    for (filter, fname) in &plugin.filters {
                        new_inner
                            .filters
                            .insert(filter.clone(), (name.clone(), fname.clone()));
                    }
                    tracing::info!(plugin = name, "plugin enabled");
                    new_inner.enabled.insert(name.clone(), plugin);
                }
                Err(e) => tracing::warn!(plugin = name, error = %e, "failed to load plugin"),
            }
        }
        *crate::utils::lock::write(&self.inner) = new_inner;
    }

    fn load_plugin(&self, name: &str) -> anyhow::Result<Plugin> {
        let dir = self.dir.join(name);
        if !dir.is_dir() {
            anyhow::bail!("plugin directory not found");
        }
        let toml_raw = std::fs::read_to_string(dir.join("plugin.toml"))?;
        let spec: PluginToml = toml::from_str(&toml_raw)?;
        if spec.meta.name.is_empty() {
            anyhow::bail!("plugin.toml is missing `name`");
        }
        let script = std::fs::read_to_string(dir.join(&spec.meta.entry))
            .map_err(|e| anyhow::anyhow!("cannot read entry {}: {e}", spec.meta.entry))?;
        // Per-plugin engine: cache and config functions are namespaced to
        // this plugin; capability APIs (network) follow the declared
        // permissions.
        let jobs = Arc::new(RwLock::new(spec.jobs));
        let mut engine = build_engine(
            self.plugin_cache.clone(),
            name,
            &self.configs,
            &spec.permissions,
            &self.instance_secret,
            self.stats.clone(),
        );
        jobs::register_api(&mut engine, name, self.scheduler.clone(), jobs.clone());
        let engine = Arc::new(engine);
        let ast = engine.compile(&script)?;
        let fns: HashSet<String> = ast.iter_functions().map(|f| f.name.to_string()).collect();

        // Plugin configuration: effective values from the ConfigManager
        // (schema defaults + config.toml + database + environment). Falls
        // back to a raw config.toml map when no namespace is loaded.
        let config = match self.configs.entry(&plugin_ns(name)) {
            Some(_) => self.configs.values_map(&plugin_ns(name)),
            None => std::fs::read_to_string(dir.join("config.toml"))
                .ok()
                .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok())
                .map(|v| toml_to_map(&v))
                .unwrap_or_default(),
        };

        // Validate route/filter targets reference existing functions.
        let routes: HashMap<String, String> = spec
            .routes
            .into_iter()
            .filter(|(path, fname)| {
                let ok = fns.contains(fname);
                if !ok {
                    tracing::warn!(plugin = name, fn = fname, route = path, "route fn not found");
                }
                ok
            })
            .collect();
        let admin_routes: HashMap<String, String> = spec
            .admin_routes
            .into_iter()
            .filter(|(path, fname)| {
                let ok = fns.contains(fname);
                if !ok {
                    tracing::warn!(plugin = name, fn = fname, route = path, "admin route fn not found");
                }
                ok
            })
            .collect();
        let filters: HashMap<String, String> = spec
            .filters
            .into_iter()
            .filter(|(filter, fname)| {
                let ok = fns.contains(fname);
                if !ok {
                    tracing::warn!(plugin = name, fn = fname, filter = filter, "filter fn not found");
                }
                ok
            })
            .collect();

        let plugin = Plugin {
            meta: spec.meta,
            permissions: spec.permissions.clone(),
            engine,
            ast,
            fns: fns.clone(),
            routes,
            admin_routes,
            filters,
            config,
            jobs,
        };

        // Lifecycle: call `init(config)` if the plugin defines it.
        if plugin.fns.contains("init") {
            let cfg = plugin.config.clone();
            if let Err(e) = plugin.engine.call_fn::<Dynamic>(
                &mut Scope::new(),
                &plugin.ast,
                "init",
                (Dynamic::from(cfg),),
            ) {
                tracing::warn!(plugin = name, error = %e, "plugin init failed");
            }
        }
        for (kind, function) in crate::utils::lock::read(&plugin.jobs).iter() {
            if !kind.starts_with(&format!("{name}.")) || !plugin.fns.contains(function) {
                anyhow::bail!("invalid plugin job registration");
            }
        }
        Ok(plugin)
    }

    pub fn generation(&self) -> u64 {
        crate::utils::lock::read(&self.inner).generation
    }

    /// All plugins found on disk: `(directory name, meta, enabled)`. The
    /// directory name is the plugin id — the key used by the enabled list,
    /// configuration namespace and extension system; `meta.name` is
    /// display-only.
    pub fn list(&self) -> Vec<(String, PluginMeta, bool)> {
        let enabled: HashSet<String> = {
            let inner = crate::utils::lock::read(&self.inner);
            inner.enabled.keys().cloned().collect()
        };
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.dir) {
            for entry in entries.flatten() {
                let toml_path = entry.path().join("plugin.toml");
                if !toml_path.is_file() {
                    continue;
                }
                let dir_name = entry.file_name().to_string_lossy().to_string();
                let meta = std::fs::read_to_string(&toml_path)
                    .ok()
                    .and_then(|raw| toml::from_str::<PluginToml>(&raw).ok())
                    .map(|spec| spec.meta);
                if let Some(mut meta) = meta {
                    if meta.name.is_empty() {
                        meta.name = dir_name.clone();
                    }
                    let is_on = enabled.contains(&dir_name);
                    out.push((dir_name, meta, is_on));
                }
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    pub fn enabled_names(&self) -> Vec<String> {
        let mut names: Vec<String> = crate::utils::lock::read(&self.inner)
            .enabled
            .keys()
            .cloned()
            .collect();
        names.sort();
        names
    }

    // -- hook dispatch ------------------------------------------------------

    /// Run a `before_*` hook: each plugin receives the map and may return a
    /// modified map. Later plugins see earlier plugins' changes.
    pub fn hook_map(&self, name: &str, map: rhai::Map) -> rhai::Map {
        let inner = crate::utils::lock::read(&self.inner);
        let mut current = map;
        for (pname, plugin) in &inner.enabled {
            if !plugin.fns.contains(name) {
                continue;
            }
            let arg = Dynamic::from(current.clone());
            match plugin
                .engine
                .call_fn::<Dynamic>(&mut Scope::new(), &plugin.ast, name, (arg,))
            {
                Ok(res) => {
                    if let Some(m) = res.try_cast::<rhai::Map>() {
                        current = m;
                    }
                }
                Err(e) => tracing::warn!(plugin = pname, hook = name, error = %e, "hook failed"),
            }
        }
        current
    }

    /// Run an `after_*` event hook: fire-and-forget, errors logged.
    pub fn hook_void(&self, name: &str, map: &rhai::Map) {
        let inner = crate::utils::lock::read(&self.inner);
        for (pname, plugin) in &inner.enabled {
            if !plugin.fns.contains(name) {
                continue;
            }
            let arg = Dynamic::from(map.clone());
            if let Err(e) =
                plugin
                    .engine
                    .call_fn::<Dynamic>(&mut Scope::new(), &plugin.ast, name, (arg,))
            {
                tracing::warn!(plugin = pname, hook = name, error = %e, "event failed");
            }
        }
    }

    /// Chain a string through plugins (e.g. markdown transforms).
    pub fn hook_str(&self, name: &str, input: &str) -> String {
        let inner = crate::utils::lock::read(&self.inner);
        let mut current = input.to_string();
        for (pname, plugin) in &inner.enabled {
            if !plugin.fns.contains(name) {
                continue;
            }
            let arg = current.clone();
            match plugin
                .engine
                .call_fn::<Dynamic>(&mut Scope::new(), &plugin.ast, name, (arg,))
            {
                Ok(res) => {
                    if let Some(s) = res.try_cast::<String>() {
                        current = s;
                    }
                }
                Err(e) => tracing::warn!(plugin = pname, hook = name, error = %e, "hook failed"),
            }
        }
        current
    }

    /// Call a registered template filter.
    pub fn call_filter(&self, filter: &str, input: &str) -> Option<String> {
        let (pname, fname) = {
            let inner = crate::utils::lock::read(&self.inner);
            inner.filters.get(filter)?.clone()
        };
        self.call_plugin_str(&pname, &fname, input)
    }

    /// Call a script function on a specific plugin, passing and returning a string.
    pub fn call_plugin_str(&self, plugin_name: &str, fn_name: &str, input: &str) -> Option<String> {
        let inner = crate::utils::lock::read(&self.inner);
        let plugin = inner.enabled.get(plugin_name)?;
        let out = plugin
            .engine
            .call_fn::<Dynamic>(
                &mut Scope::new(),
                &plugin.ast,
                fn_name,
                (input.to_string(),),
            )
            .ok()?;
        Some(out.to_string())
    }

    /// Registered template filters (for Tera registration on theme load).
    pub fn filters(&self) -> Vec<(String, String, String)> {
        // (filter_name, plugin_name, fn_name)
        let inner = crate::utils::lock::read(&self.inner);
        inner
            .filters
            .iter()
            .map(|(f, (p, n))| (f.clone(), p.clone(), n.clone()))
            .collect()
    }

    /// Admin routes registered by enabled plugins: `(plugin_name, path)`.
    /// Used to render navigation links to plugin-provided admin pages.
    pub fn admin_route_paths(&self) -> Vec<(String, String)> {
        let inner = crate::utils::lock::read(&self.inner);
        let mut out: Vec<(String, String)> = inner
            .enabled
            .iter()
            .flat_map(|(name, p)| p.admin_routes.keys().map(|k| (name.clone(), k.clone())))
            .collect();
        out.sort();
        out
    }

    /// Navigation items contributed by plugins (hook `nav` returning an array
    /// of maps with `label` and `url`).
    pub fn nav_items(&self) -> Vec<(String, String)> {
        let inner = crate::utils::lock::read(&self.inner);
        let mut out = Vec::new();
        for (pname, plugin) in &inner.enabled {
            if !plugin.fns.contains("nav") {
                continue;
            }
            match plugin
                .engine
                .call_fn::<Dynamic>(&mut Scope::new(), &plugin.ast, "nav", ())
            {
                Ok(res) => {
                    if let Some(arr) = res.try_cast::<rhai::Array>() {
                        for item in arr {
                            if let Some(m) = item.try_cast::<rhai::Map>() {
                                let label = m
                                    .get("label")
                                    .and_then(|d| d.clone().try_cast::<String>())
                                    .unwrap_or_default();
                                let url = m
                                    .get("url")
                                    .and_then(|d| d.clone().try_cast::<String>())
                                    .unwrap_or_default();
                                if !label.is_empty() && !url.is_empty() {
                                    out.push((label, url));
                                }
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(plugin = pname, error = %e, "nav hook failed"),
            }
        }
        out
    }

    /// Invoke a plugin public route. `url_path` is the full request URL
    /// (e.g. `/plugins/greet`); registered routes are relative (`/greet`).
    pub fn route(&self, url_path: &str, query: &rhai::Map) -> Option<RouteResult> {
        self.run_route(url_path, query, false)
    }

    /// Run the `request_guard` hook chain (permission `request.guard`).
    ///
    /// Each guard plugin receives a request map `{method, path, query,
    /// cookies, ip, user_agent, has_session}` and may return:
    /// * `()` — allow;
    /// * `#{redirect: "…"}` — 303 to the given location;
    /// * `#{status: 403, body: "…"}` — a direct response.
    ///
    /// The first verdict wins; plugins without the hook, without the
    /// `request.guard` permission, or with failing scripts mean "allow" —
    /// the guard is a hardening layer, never a hard dependency.
    pub fn request_guard(&self, req: rhai::Map) -> GuardOutcome {
        let inner = crate::utils::lock::read(&self.inner);
        for (pname, plugin) in &inner.enabled {
            if !plugin.fns.contains("request_guard")
                || !plugin
                    .permissions
                    .iter()
                    .any(|p| p == REQUEST_GUARD_PERMISSION)
            {
                continue;
            }
            match plugin.engine.call_fn::<Dynamic>(
                &mut Scope::new(),
                &plugin.ast,
                "request_guard",
                (Dynamic::from(req.clone()),),
            ) {
                Ok(d) if d.is_unit() => continue, // allow
                Ok(d) if d.is::<rhai::Map>() => {
                    let m = d.try_cast::<rhai::Map>().expect("checked is::<Map>");
                    if let Some(loc) = m
                        .get("redirect")
                        .and_then(|v| v.clone().try_cast::<String>())
                        .filter(|s| s.starts_with('/'))
                    {
                        return GuardOutcome::Redirect(loc);
                    }
                    let status = m
                        .get("status")
                        .and_then(|v| v.clone().try_cast::<i64>())
                        .unwrap_or(403);
                    let status = u16::try_from(status).unwrap_or(403);
                    let body = m
                        .get("body")
                        .and_then(|v| v.clone().try_cast::<String>())
                        .unwrap_or_default();
                    return GuardOutcome::Respond { status, body };
                }
                Ok(_) => continue, // unexpected return type — treat as allow
                Err(e) => {
                    tracing::warn!(plugin = pname, hook = "request_guard", error = %e, "guard failed");
                    continue;
                }
            }
        }
        GuardOutcome::Allow
    }

    /// Invoke a plugin admin route (requires an authenticated session).
    /// `url_path` is the full request URL (e.g. `/admin/plugins/stats`).
    pub fn admin_route(
        &self,
        url_path: &str,
        query: &rhai::Map,
        user: &crate::auth::AuthCtx,
    ) -> Option<RouteResult> {
        let mut ctx = query.clone();
        ctx.insert(
            "user".into(),
            Dynamic::from(crate::rmap! {
                "username" => user.username.clone(),
                "role" => user.role.as_str().to_string(),
            }),
        );
        self.run_route(url_path, &ctx, true)
    }

    fn run_route(&self, url_path: &str, ctx: &rhai::Map, admin: bool) -> Option<RouteResult> {
        let inner = crate::utils::lock::read(&self.inner);
        // Route keys may be registered as full paths ("/plugins/greet") or
        // mount-relative ("/greet"); `ctx.path` always exposes the full URL.
        let relative = url_path
            .strip_prefix("/admin/plugins")
            .or_else(|| url_path.strip_prefix("/plugins"))
            .unwrap_or(url_path);
        let mut candidates: Vec<&str> = vec![url_path, relative];
        if let Some(rest) = url_path.strip_prefix("/admin") {
            candidates.push(rest);
        }
        for (pname, plugin) in &inner.enabled {
            let table = if admin {
                &plugin.admin_routes
            } else {
                &plugin.routes
            };
            let Some(fname) = candidates.iter().find_map(|k| table.get(*k)) else {
                continue;
            };
            let mut call_ctx = ctx.clone();
            call_ctx.insert("path".into(), Dynamic::from(url_path.to_string()));
            // Site locale — plugins can localize their pages.
            call_ctx.insert("lang".into(), Dynamic::from(crate::i18n::locale()));
            let res = plugin.engine.call_fn::<Dynamic>(
                &mut Scope::new(),
                &plugin.ast,
                fname,
                (Dynamic::from(call_ctx),),
            );
            match res {
                Ok(d) => return Some(route_result_from_dynamic(d)),
                Err(e) => {
                    tracing::warn!(plugin = pname, path = url_path, error = %e, "plugin route failed");
                    return Some(RouteResult {
                        status: 500,
                        content_type: "text/plain; charset=utf-8".into(),
                        body: "plugin error".into(),
                    });
                }
            }
        }
        None
    }
}

fn route_result_from_dynamic(d: Dynamic) -> RouteResult {
    if d.is::<rhai::Map>() {
        let m = d.try_cast::<rhai::Map>().expect("checked is::<Map>");
        let status = m
            .get("status")
            .and_then(|d| d.clone().try_cast::<i64>())
            .unwrap_or(200);
        let body = m
            .get("body")
            .and_then(|d| d.clone().try_cast::<String>())
            .unwrap_or_default();
        let content_type = m
            .get("content_type")
            .and_then(|d| d.clone().try_cast::<String>())
            .unwrap_or_else(|| "text/html; charset=utf-8".into());
        return RouteResult {
            status: status.clamp(200, 599) as u16,
            content_type,
            body,
        };
    }
    if d.is::<String>() {
        let s = d.try_cast::<String>().expect("checked is::<String>");
        return RouteResult {
            status: 200,
            content_type: "text/html; charset=utf-8".into(),
            body: s,
        };
    }
    RouteResult {
        status: 200,
        content_type: "text/plain; charset=utf-8".into(),
        body: d.to_string(),
    }
}

/// Resource limits for the Rhai sandbox.
///
/// Without them a buggy or hostile script (`loop {}`, a 100 MB string, …)
/// would spin a tokio worker thread forever or balloon memory — hooks and
/// template filters run synchronously inside request paths, so that hangs
/// the whole server. Limits are generous for legitimate plugins (a hook
/// typically runs in well under a millisecond) but hard.
const RHAI_MAX_OPERATIONS: u64 = 1_000_000;
const RHAI_MAX_STRING_SIZE: usize = 2 * 1024 * 1024;
const RHAI_MAX_ARRAY_SIZE: usize = 10_000;
const RHAI_MAX_MAP_SIZE: usize = 1_000;
const RHAI_MAX_CALL_LEVELS: usize = 64;
const RHAI_MAX_MODULES: usize = 16;

fn build_engine(
    plugin_cache: Option<Arc<MemoryCache>>,
    plugin_name: &str,
    configs: &Arc<ConfigManager>,
    permissions: &[String],
    instance_secret: &str,
    stats: std::sync::Arc<crate::plugins::stats::Stats>,
) -> Engine {
    let mut engine = Engine::new();

    // Resource limits: every script execution is bounded in operations,
    // memory and call depth. A runaway script now fails with an error
    // (logged, hook skipped) instead of hanging a request thread.
    engine.set_max_operations(RHAI_MAX_OPERATIONS);
    engine.set_max_string_size(RHAI_MAX_STRING_SIZE);
    engine.set_max_array_size(RHAI_MAX_ARRAY_SIZE);
    engine.set_max_map_size(RHAI_MAX_MAP_SIZE);
    engine.set_max_call_levels(RHAI_MAX_CALL_LEVELS);
    engine.set_max_modules(RHAI_MAX_MODULES);
    engine.set_max_expr_depths(32, 64);

    // JSON helpers — pure functions, always available. Most HTTP APIs speak
    // JSON, so `json_parse` pairs with the network API below (but is useful
    // on its own).
    engine.register_fn(
        "json_parse",
        |text: &str| -> Result<Dynamic, Box<EvalAltResult>> {
            serde_json::from_str::<serde_json::Value>(text)
                .map(|v| json_to_dynamic(&v))
                .map_err(|e| e.to_string().into())
        },
    );
    engine.register_fn(
        "json_stringify",
        |value: Dynamic| -> Result<String, Box<EvalAltResult>> {
            serde_json::to_string(&dynamic_to_json(&value)).map_err(|e| e.to_string().into())
        },
    );

    // Sandboxed host functions only — no filesystem or process access;
    // network access is exposed exclusively through the permission-gated
    // `http_*` API (`network.fetch`, see `http.rs`).
    engine.register_fn("log", |level: &str, msg: &str| match level {
        "debug" => tracing::debug!(target: "polaris::plugin", "{msg}"),
        "warn" => tracing::warn!(target: "polaris::plugin", "{msg}"),
        "error" => tracing::error!(target: "polaris::plugin", "{msg}"),
        _ => tracing::info!(target: "polaris::plugin", "{msg}"),
    });
    engine.register_fn("now", crate::utils::time::now);

    // Statistics counters — atomic, in-memory, TTL'd per key (visitor
    // counts, event tallies). Reset on restart by design.
    let stats_incr = stats.clone();
    engine.register_fn("stat_incr", move |key: &str, ttl_secs: i64| -> i64 {
        stats_incr.incr(
            key,
            std::time::Duration::from_secs(ttl_secs.clamp(60, 86_400 * 7) as u64),
        )
    });
    let stats = stats.clone();
    let stats = stats.clone();
    engine.register_fn("stat_get", move |key: &str| -> i64 { stats.get(key) });

    // Plugin-scoped signing key: HMAC(instance secret, "plugin-signing:name")
    // — domain-separated, so a plugin can sign its own tokens (e.g. gate
    // clearance cookies) without ever seeing the instance secret.
    crypto::register_signing(&mut engine, plugin_name, instance_secret);
    engine.register_fn("now_iso", || {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    });

    // Crypto & encoding helpers — pure functions (webhook signing, Basic
    // auth, data URIs, URL building). No permission gate: nothing here
    // performs I/O.
    crypto::register(&mut engine);

    // Plugin cache API. Values live under `plugin:{name}:*` — one plugin
    // can never read, overwrite or pollute another plugin's (or the
    // engine's) cache entries. Always in-memory, synchronous, no network.
    if let Some(cache) = plugin_cache {
        let ns = format!("plugin:{plugin_name}:");

        let c = cache.clone();
        let ns_get = ns.clone();
        engine.register_fn("cache_get", move |key: &str| -> String {
            c.get_sync(&format!("{ns_get}{key}"))
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        });

        let c = cache.clone();
        let ns_set = ns.clone();
        engine.register_fn("cache_set", move |key: &str, value: &str, ttl_secs: i64| {
            c.set_sync(
                &format!("{ns_set}{key}"),
                Arc::new(value.as_bytes().to_vec()),
                Duration::from_secs(ttl_secs.clamp(1, 86_400 * 365) as u64),
            );
        });

        let c = cache.clone();
        let ns_del = ns;
        engine.register_fn("cache_del", move |key: &str| {
            c.del_sync(&format!("{ns_del}{key}"));
        });
    }

    // Capability API: outbound HTTP, granted only when the manifest declares
    // the matching permission. Registered functions are SSRF-guarded and
    // size/time-capped — see `http.rs`.
    if permissions.iter().any(|p| p == http::PERMISSION) {
        http::register(&mut engine);
    }

    // Configuration API — strictly read-only, namespaced to this plugin.
    // Scripts can never touch core, theme or other plugins' configuration.
    let ns = plugin_ns(plugin_name);

    let c = configs.clone();
    let ns_str = ns.clone();
    engine.register_fn("config_get", move |key: &str| -> String {
        c.get_string(&ns_str, key).unwrap_or_default()
    });

    let c = configs.clone();
    let ns_str = ns.clone();
    engine.register_fn("config_get_bool", move |key: &str| -> bool {
        c.get_bool(&ns_str, key).unwrap_or(false)
    });

    let c = configs.clone();
    let ns_str = ns.clone();
    engine.register_fn("config_get_int", move |key: &str| -> i64 {
        c.get_int(&ns_str, key).unwrap_or(0)
    });

    let c = configs.clone();
    let ns_str = ns;
    engine.register_fn("config_has", move |key: &str| -> bool {
        c.get(&ns_str, key).is_some()
    });

    engine
}

// ---------------------------------------------------------------------------
// rhai <-> JSON conversions
// ---------------------------------------------------------------------------

fn toml_to_map(v: &toml::Value) -> rhai::Map {
    let mut map = rhai::Map::new();
    if let toml::Value::Table(t) = v {
        for (k, val) in t {
            let d = match val {
                toml::Value::String(s) => Dynamic::from(s.clone()),
                toml::Value::Integer(i) => Dynamic::from(*i),
                toml::Value::Float(f) => Dynamic::from(*f),
                toml::Value::Boolean(b) => Dynamic::from(*b),
                toml::Value::Table(t) => Dynamic::from(toml_to_map(&toml::Value::Table(t.clone()))),
                _ => Dynamic::UNIT,
            };
            map.insert(k.as_str().into(), d);
        }
    }
    map
}

pub fn json_to_map(v: &serde_json::Value) -> rhai::Map {
    let mut map = rhai::Map::new();
    if let serde_json::Value::Object(o) = v {
        for (k, val) in o {
            map.insert(k.as_str().into(), json_to_dynamic(val));
        }
    }
    map
}

fn json_to_dynamic(v: &serde_json::Value) -> Dynamic {
    match v {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => Dynamic::from(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Dynamic::from(i)
            } else {
                Dynamic::from(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => Dynamic::from(s.clone()),
        serde_json::Value::Array(a) => {
            Dynamic::from(a.iter().map(json_to_dynamic).collect::<rhai::Array>())
        }
        serde_json::Value::Object(o) => {
            Dynamic::from(json_to_map(&serde_json::Value::Object(o.clone())))
        }
    }
}

pub fn map_to_json(m: &rhai::Map) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    for (k, v) in m {
        o.insert(k.to_string(), dynamic_to_json(v));
    }
    serde_json::Value::Object(o)
}

fn dynamic_to_json(v: &Dynamic) -> serde_json::Value {
    if v.is_unit() {
        return serde_json::Value::Null;
    }
    if let Ok(b) = v.as_bool() {
        return serde_json::Value::Bool(b);
    }
    if let Ok(i) = v.as_int() {
        return serde_json::json!(i);
    }
    if let Ok(f) = v.as_float() {
        return serde_json::json!(f);
    }
    if v.is::<rhai::ImmutableString>() {
        return serde_json::Value::String(v.to_string());
    }
    if v.is::<rhai::Array>()
        && let Some(arr) = v.clone().try_cast::<rhai::Array>()
    {
        return serde_json::Value::Array(arr.iter().map(dynamic_to_json).collect());
    }
    if v.is::<rhai::Map>()
        && let Some(m) = v.clone().try_cast::<rhai::Map>()
    {
        return map_to_json(&m);
    }
    serde_json::Value::String(v.to_string())
}

/// Helper used by services: run `before_*` hook over a JSON object.
/// Plugin-returned values are merged back over the input.
pub fn hook_json(app: &crate::state::App, hook: &str, value: &mut serde_json::Value) {
    let map = json_to_map(value);
    let out = app.plugins.hook_map(hook, map);
    let merged = map_to_json(&out);
    if let (serde_json::Value::Object(target), serde_json::Value::Object(src)) = (value, merged) {
        for (k, v) in src {
            target.insert(k, v);
        }
    }
}

/// Fire-and-forget event over a JSON object.
pub fn event_json(app: &crate::state::App, hook: &str, value: &serde_json::Value) {
    let map = json_to_map(value);
    app.plugins.hook_void(hook, &map);
}

/// Convenience macro-free map builder.
#[macro_export]
macro_rules! rmap {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut m = rhai::Map::new();
        $( m.insert($k.into(), rhai::Dynamic::from($v)); )*
        m
    }};
}
