//! ConfigManager — runtime store for theme/plugin configuration.
//!
//! Layering (later wins):
//! ```text
//! schema default  →  config.toml (file default)  →  database  →  environment
//! ```
//!
//! - Namespaces: `theme.<name>` and `plugin.<name>`; database keys are
//!   `<namespace>.<key>` in the existing `settings` table (one portable
//!   store for SQLite / MySQL / PostgreSQL, no new migration).
//! - Values are cached in memory (`RwLock`), so template rendering and Rhai
//!   plugin scripts read configuration synchronously without touching the
//!   database. Saving validates, persists, refreshes the in-memory snapshot
//!   and reports the changes so callers can invalidate caches and fire
//!   `on_config_changed` hooks.
//! - Sensitive fields (password/api_key/secret/token/private_key) are
//!   AES-256-GCM encrypted at rest with a key derived from the configured
//!   secret, masked in the admin UI and excluded from template contexts.
//! - A namespace only accepts keys declared by its schema: plugins can never
//!   write `core.*`, `theme.*` or another plugin's keys (no write API is
//!   exposed to scripts at all).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use sqlx::Row;

use crate::config_schema::{self, ConfigValue, FieldDef, SchemaDef, ShowIf};
use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::utils::cookies;

const ENC_PREFIX: &str = "enc:v1:";

// ---------------------------------------------------------------------------
// Namespace helpers
// ---------------------------------------------------------------------------

/// Sanitize a theme/plugin name for environment variable segments:
/// `polaris-default` → `POLARIS_DEFAULT`.
fn env_segment(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub fn theme_ns(theme: &str) -> String {
    format!("theme.{theme}")
}

pub fn plugin_ns(plugin: &str) -> String {
    format!("plugin.{plugin}")
}

fn ns_kind(ns: &str) -> &str {
    ns.split_once('.').map(|(kind, _)| kind).unwrap_or(ns)
}

fn ns_name(ns: &str) -> &str {
    ns.split_once('.').map(|(_, name)| name).unwrap_or(ns)
}

// ---------------------------------------------------------------------------
// In-memory snapshot
// ---------------------------------------------------------------------------

pub struct NamespaceEntry {
    pub schema: Arc<SchemaDef>,
    /// Effective values (env > db > file default > schema default), including
    /// decrypted secrets — never hand this map to a template directly.
    pub effective: Arc<BTreeMap<String, ConfigValue>>,
    /// Decrypted database overlay (`<ns>.<key>` rows).
    db_values: BTreeMap<String, ConfigValue>,
}

impl NamespaceEntry {
    /// Should this field be rendered on the settings page right now?
    /// (`show_if` evaluated against the current effective values.)
    pub fn show_if_visible(&self, key: &str) -> bool {
        let Some(field) = self.schema.field(key) else {
            return true;
        };
        let Some(expr) = &field.show_if else {
            return true;
        };
        match ShowIf::parse(expr) {
            Ok(e) => e.eval(&self.effective),
            Err(_) => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Change events
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct ConfigChange {
    pub namespace: String,
    pub key: String,
    pub old_value: Option<ConfigValue>,
    pub new_value: Option<ConfigValue>,
}

impl ConfigChange {
    pub fn to_event_map(&self) -> rhai::Map {
        crate::rmap! {
            "namespace" => self.namespace.clone(),
            "key" => self.key.clone(),
            "old_value" => self.old_value.as_ref().map(|v| v.to_cmp_string()).unwrap_or_default(),
            "new_value" => self.new_value.as_ref().map(|v| v.to_cmp_string()).unwrap_or_default(),
        }
    }
}

#[derive(Debug, Default)]
pub struct SaveOutcome {
    pub changes: Vec<ConfigChange>,
    pub restart_required: bool,
}

// ---------------------------------------------------------------------------
// Encryption (AES-256-GCM, key = SHA-256(secret))
// ---------------------------------------------------------------------------

fn derive_key(secret: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.update(b"polaris-config-v1");
    h.finalize().into()
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for i in (0..bytes.len()).step_by(2) {
        let hi = (bytes[i] as char).to_digit(16)?;
        let lo = (bytes[i + 1] as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
    }
    Some(out)
}

fn random_nonce() -> [u8; 12] {
    let token = cookies::random_token(12);
    let bytes = hex_decode(&token).expect("hex round-trip");
    let mut nonce = [0u8; 12];
    nonce.copy_from_slice(&bytes);
    nonce
}

fn encrypt_value(secret: &str, plain: &str) -> AppResult<String> {
    let key = Key::<Aes256Gcm>::from(derive_key(secret));
    let cipher = Aes256Gcm::new(&key);
    let nonce_raw = random_nonce();
    let nonce = Nonce::from_slice(&nonce_raw);
    let ct = cipher
        .encrypt(nonce, plain.as_bytes())
        .map_err(|e| crate::error::AppError::Internal(anyhow::anyhow!("encrypt failed: {e}")))?;
    Ok(format!(
        "{ENC_PREFIX}{}{}",
        hex_encode(&nonce_raw),
        hex_encode(&ct)
    ))
}

fn decrypt_value(secret: &str, stored: &str) -> Option<String> {
    let payload = stored.strip_prefix(ENC_PREFIX)?;
    if payload.len() < 24 {
        return None;
    }
    let nonce_hex = payload.get(..24)?;
    let ct_hex = payload.get(24..)?;
    let nonce_raw = hex_decode(nonce_hex)?;
    let ct = hex_decode(ct_hex)?;
    let key = Key::<Aes256Gcm>::from(derive_key(secret));
    let cipher = Aes256Gcm::new(&key);
    let nonce = Nonce::from_slice(&nonce_raw);
    let plain = cipher.decrypt(nonce, ct.as_ref()).ok()?;
    String::from_utf8(plain).ok()
}

/// Decrypt a stored value: encrypted entries are unwrapped, anything else is
/// returned as-is (accepts values written before encryption was enabled).
fn unwrap_stored(secret: &str, stored: &str) -> Result<String, String> {
    if stored.starts_with("enc:") {
        return decrypt_value(secret, stored).ok_or_else(|| {
            "cannot decrypt stored configuration; check security.secret or restore the original key".into()
        });
    }
    Ok(stored.to_string())
}

// ---------------------------------------------------------------------------
// ConfigManager
// ---------------------------------------------------------------------------

pub struct ConfigManager {
    db: Db,
    secret: String,
    /// Raw `POLARIS_THEME_*` / `POLARIS_PLUGIN_*` variables, resolved against
    /// a schema when its namespace loads.
    env_vars: Vec<(String, String)>, // ("<THEME>_<NAME>_<KEY>", value)
    namespaces: std::sync::RwLock<HashMap<String, Arc<NamespaceEntry>>>,
}

impl ConfigManager {
    /// Check encrypted backup values before changing files or database rows.
    pub(crate) fn validate_stored_secret(&self, stored: &str) -> Result<(), String> {
        unwrap_stored(&self.secret, stored).map(|_| ())
    }

    pub fn new(db: Db, secret: String) -> Self {
        let env_vars = std::env::vars()
            .filter_map(|(k, v)| {
                let (rest, kind) = k
                    .strip_prefix("POLARIS_THEME_")
                    .map(|r| (r.to_string(), "theme"))
                    .or_else(|| {
                        k.strip_prefix("POLARIS_PLUGIN_")
                            .map(|r| (r.to_string(), "plugin"))
                    })?;
                if rest.is_empty() {
                    return None;
                }
                Some((format!("{}_{}", env_segment(kind), rest), v))
            })
            .collect();
        Self {
            db,
            secret,
            env_vars,
            namespaces: Default::default(),
        }
    }

    /// Resolve environment overrides for a namespace: `POLARIS_THEME_DEFAULT_
    /// SITE_TITLE` → theme `default`, key `site_title`.
    fn env_overrides(
        &self,
        kind: &str,
        name: &str,
        schema: &SchemaDef,
    ) -> BTreeMap<String, ConfigValue> {
        let prefix = format!("{}_{}", env_segment(kind), env_segment(name));
        let mut out = BTreeMap::new();
        for (rest, value) in &self.env_vars {
            let Some(key_upper) = rest.strip_prefix(&format!("{prefix}_")) else {
                continue;
            };
            let key = key_upper.to_ascii_lowercase();
            let Some(field) = schema.field(&key) else {
                continue;
            };
            match config_schema::parse_input(field, value) {
                Ok(Some(v)) => {
                    out.insert(key, v);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(env_key = key, error = %e, "invalid config env override ignored")
                }
            }
        }
        out
    }

    /// Register (or refresh) a namespace from its schema and file defaults,
    /// loading persisted values from the database. Called at startup, when a
    /// theme/plugin is activated, and when its settings page is opened
    /// (picks up schema file edits).
    pub async fn load_namespace(
        &self,
        ns: &str,
        schema: SchemaDef,
        file_defaults: BTreeMap<String, ConfigValue>,
    ) -> Result<Arc<NamespaceEntry>, String> {
        let schema = Arc::new(schema);
        let prefix = format!("{ns}.");
        let mut db_values: BTreeMap<String, ConfigValue> = BTreeMap::new();
        if let Ok(rows) = self
            .db
            .fetch_all("SELECT name, value FROM settings", &[])
            .await
        {
            for r in rows {
                let name: String = r.try_get("name").unwrap_or_default();
                let Some(key) = name.strip_prefix(&prefix) else {
                    continue;
                };
                let stored: String = crate::db::text(&r, "value")
                    .map_err(|_| format!("cannot decode stored configuration {name}"))?;
                let Some(field) = schema.field(key) else {
                    continue;
                };
                let plain = if field.sensitive() {
                    unwrap_stored(&self.secret, &stored)
                        .map_err(|error| format!("{ns}.{key}: {error}"))?
                } else {
                    stored
                };
                match config_schema::parse_input(field, &plain) {
                    Ok(Some(v)) => {
                        db_values.insert(key.to_string(), v);
                    }
                    _ => {
                        tracing::warn!(namespace = ns, key, "skipping invalid stored config value")
                    }
                }
            }
        }

        let env = self.env_overrides(ns_kind(ns), ns_name(ns), &schema);
        let effective = compute_effective(&schema, &file_defaults, &db_values, &env);
        let entry = Arc::new(NamespaceEntry {
            schema,
            effective: Arc::new(effective),
            db_values,
        });
        self.namespaces
            .write()
            .unwrap()
            .insert(ns.to_string(), entry.clone());
        Ok(entry)
    }

    /// Drop a namespace's cached entry (e.g. plugin disabled).
    pub fn unload_namespace(&self, ns: &str) {
        crate::utils::lock::write(&self.namespaces).remove(ns);
    }

    /// Load the configuration namespace for a theme directory
    /// (`<themes_dir>/<name>/config.schema.toml` + optional `config.toml`
    /// file defaults). A theme without a schema gets an empty one.
    pub async fn load_theme(
        &self,
        themes_dir: &std::path::Path,
        name: &str,
    ) -> Result<Arc<NamespaceEntry>, String> {
        let dir = themes_dir.join(name);
        let schema = config_schema::load_schema_file(&dir.join("config.schema.toml"))?;
        let file_defaults = std::fs::read_to_string(dir.join("config.toml"))
            .ok()
            .map(|raw| parse_file_defaults(&schema, &raw))
            .unwrap_or_default();
        self.load_namespace(&theme_ns(name), schema, file_defaults)
            .await
    }

    /// Load the configuration namespace for a plugin directory
    /// (`<plugins_dir>/<name>/config.schema.toml` + optional `config.toml`).
    pub async fn load_plugin(
        &self,
        plugins_dir: &std::path::Path,
        name: &str,
    ) -> Result<Arc<NamespaceEntry>, String> {
        let dir = plugins_dir.join(name);
        let schema = config_schema::load_schema_file(&dir.join("config.schema.toml"))?;
        let file_defaults = std::fs::read_to_string(dir.join("config.toml"))
            .ok()
            .map(|raw| parse_file_defaults(&schema, &raw))
            .unwrap_or_default();
        self.load_namespace(&plugin_ns(name), schema, file_defaults)
            .await
    }

    /// Does a theme/plugin on disk declare any configurable fields?
    pub fn has_schema(base_dir: &std::path::Path, name: &str) -> bool {
        config_schema::load_schema_file(&base_dir.join(name).join("config.schema.toml"))
            .map(|s| !s.fields.is_empty())
            .unwrap_or(false)
    }

    pub fn entry(&self, ns: &str) -> Option<Arc<NamespaceEntry>> {
        crate::utils::lock::read(&self.namespaces).get(ns).cloned()
    }

    pub fn loaded_namespaces(&self) -> Vec<String> {
        crate::utils::lock::read(&self.namespaces)
            .keys()
            .cloned()
            .collect()
    }

    // -- typed reads (sync — hot path for templates and Rhai) ----------------

    /// Effective value of one key (secrets included — callers decide exposure).
    pub fn get(&self, ns: &str, key: &str) -> Option<ConfigValue> {
        self.entry(ns)?.effective.get(key).cloned()
    }

    pub fn get_string(&self, ns: &str, key: &str) -> Option<String> {
        match self.get(ns, key)? {
            ConfigValue::Str(s) => Some(s),
            other => Some(other.to_cmp_string()),
        }
    }

    pub fn get_bool(&self, ns: &str, key: &str) -> Option<bool> {
        match self.get(ns, key)? {
            ConfigValue::Bool(b) => Some(b),
            ConfigValue::Str(s) => Some(s == "true"),
            ConfigValue::Int(i) => Some(i != 0),
            _ => None,
        }
    }

    pub fn get_int(&self, ns: &str, key: &str) -> Option<i64> {
        match self.get(ns, key)? {
            ConfigValue::Int(i) => Some(i),
            ConfigValue::Float(f) => Some(f as i64),
            ConfigValue::Str(s) => s.parse().ok(),
            ConfigValue::Bool(b) => Some(b as i64),
            _ => None,
        }
    }

    /// Values as JSON for template contexts. Secrets are replaced with `null`
    /// unless revealed (admin API).
    pub fn values_json(&self, ns: &str, reveal_secrets: bool) -> serde_json::Value {
        let Some(entry) = self.entry(ns) else {
            return serde_json::Value::Null;
        };
        let mut map = serde_json::Map::new();
        for (key, v) in entry.effective.iter() {
            let is_secret = entry
                .schema
                .field(key)
                .map(FieldDef::sensitive)
                .unwrap_or(false);
            if is_secret && !reveal_secrets {
                map.insert(key.clone(), serde_json::Value::Null);
            } else {
                map.insert(key.clone(), v.to_json());
            }
        }
        serde_json::Value::Object(map)
    }

    /// Effective value map for Rhai `init(config)` — plugins may read their
    /// own secrets, so they are included.
    pub fn values_map(&self, ns: &str) -> rhai::Map {
        let Some(entry) = self.entry(ns) else {
            return rhai::Map::new();
        };
        let mut m = rhai::Map::new();
        for (k, v) in entry.effective.iter() {
            m.insert(k.as_str().into(), rhai::Dynamic::from(v.to_cmp_string()));
        }
        m
    }

    // -- persistence ----------------------------------------------------------

    /// Validate and persist a batch of raw form values for a namespace.
    ///
    /// * Only schema-declared keys are accepted — everything else is an error
    ///   (this is the namespace isolation boundary).
    /// * Empty input for a non-secret field removes the override (falls back
    ///   to defaults); for secrets it means "keep current value".
    /// * Required fields are validated against the post-save effective value.
    /// * Sensitive values are encrypted before hitting the database.
    pub async fn save(
        &self,
        ns: &str,
        input: &HashMap<String, String>,
        actor_permission: config_schema::Permission,
    ) -> Result<SaveOutcome, String> {
        let entry = self
            .entry(ns)
            .ok_or_else(|| format!("unknown config namespace '{ns}'"))?;

        // 1. Parse, validate and permission-check every submitted field.
        let mut parsed: BTreeMap<String, Option<ConfigValue>> = BTreeMap::new();
        for (key, raw) in input {
            let field = entry
                .schema
                .field(key)
                .ok_or_else(|| format!("'{key}' is not a configurable key of {ns}"))?;
            if field.permission > actor_permission {
                return Err(format!(
                    "'{}' requires {} permission",
                    field.label,
                    field.permission.as_str()
                ));
            }
            if field.sensitive() && raw.trim().is_empty() {
                continue; // keep current value
            }
            parsed.insert(key.clone(), config_schema::parse_input(field, raw)?);
        }

        // 2. Build the next database overlay.
        let mut next_db = entry.db_values.clone();
        for (key, v) in &parsed {
            match v {
                Some(v) => {
                    next_db.insert(key.clone(), v.clone());
                }
                None => {
                    next_db.remove(key);
                }
            }
        }

        let env = self.env_overrides(ns_kind(ns), ns_name(ns), &entry.schema);
        let next_effective = compute_effective(&entry.schema, &BTreeMap::new(), &next_db, &env);

        // 3. Required check against the resulting effective values.
        for field in &entry.schema.fields {
            if !field.required {
                continue;
            }
            let empty = next_effective
                .get(&field.key)
                .map(ConfigValue::is_empty)
                .unwrap_or(true);
            if empty {
                return Err(format!("{} is required", field.label));
            }
        }

        // 4. Persist: upserts for new/changed keys, deletes for removed ones.
        for (key, v) in &next_db {
            if entry.db_values.get(key) == Some(v) {
                continue; // unchanged — skip the write
            }
            let field = entry.schema.field(key).expect("validated above");
            let stored = if field.sensitive() {
                encrypt_value(&self.secret, &v.to_store())
                    .map_err(|e| format!("cannot encrypt '{}': {e}", field.label))?
            } else {
                v.to_store()
            };
            self.db
                .upsert_setting(&format!("{ns}.{key}"), &stored)
                .await
                .map_err(|e| format!("persist failed: {e}"))?;
        }
        for key in entry.db_values.keys() {
            if !next_db.contains_key(key) {
                self.db
                    .execute(
                        "DELETE FROM settings WHERE name = ?",
                        &[Bind::S(format!("{ns}.{key}"))],
                    )
                    .await
                    .map_err(|e| format!("persist failed: {e}"))?;
            }
        }

        // 5. Diff old vs new effective values and refresh the snapshot.
        let mut changes = Vec::new();
        let mut restart_required = false;
        let keys: HashSet<&String> = entry
            .effective
            .keys()
            .chain(next_effective.keys())
            .collect();
        for key in keys {
            let old = entry.effective.get(key);
            let new = next_effective.get(key);
            if old != new {
                if let Some(f) = entry.schema.field(key)
                    && f.restart
                {
                    restart_required = true;
                }
                changes.push(ConfigChange {
                    namespace: ns.to_string(),
                    key: key.clone(),
                    old_value: old.cloned(),
                    new_value: new.cloned(),
                });
            }
        }

        let refreshed = Arc::new(NamespaceEntry {
            schema: entry.schema.clone(),
            effective: Arc::new(next_effective),
            db_values: next_db,
        });
        self.namespaces
            .write()
            .unwrap()
            .insert(ns.to_string(), refreshed);

        Ok(SaveOutcome {
            changes,
            restart_required,
        })
    }
}

fn compute_effective(
    schema: &SchemaDef,
    file_defaults: &BTreeMap<String, ConfigValue>,
    db_values: &BTreeMap<String, ConfigValue>,
    env: &BTreeMap<String, ConfigValue>,
) -> BTreeMap<String, ConfigValue> {
    let mut out = BTreeMap::new();
    for field in &schema.fields {
        let v = env
            .get(&field.key)
            .or_else(|| db_values.get(&field.key))
            .or_else(|| file_defaults.get(&field.key))
            .or(field.default.as_ref());
        if let Some(v) = v {
            out.insert(field.key.clone(), v.clone());
        }
    }
    out
}

/// Parse a `config.toml` (file-level defaults) against a schema. Only
/// schema-declared, flat keys are honoured; nested tables are skipped.
pub fn parse_file_defaults(schema: &SchemaDef, raw: &str) -> BTreeMap<String, ConfigValue> {
    let mut out = BTreeMap::new();
    let Ok(root) = toml::from_str::<toml::Value>(raw) else {
        return out;
    };
    let Some(table) = root.as_table() else {
        return out;
    };
    for (key, val) in table {
        let Some(field) = schema.field(key) else {
            continue;
        };
        let raw_str = match val {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::Boolean(b) => b.to_string(),
            _ => continue,
        };
        if let Ok(Some(v)) = config_schema::parse_input(field, &raw_str) {
            out.insert(key.clone(), v);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = r#"
[site_title]
type = "string"
label = "Site title"
default = "Polaris Blog"

[api_key]
type = "password"
label = "API key"

[posts_per_page]
type = "integer"
label = "Posts per page"
default = 10
min = 1
max = 50
"#;

    fn schema() -> SchemaDef {
        config_schema::parse_schema(SCHEMA).unwrap()
    }

    #[test]
    fn encryption_round_trip() {
        let secret = "s3cret";
        let enc = encrypt_value(secret, "hunter2").unwrap();
        assert!(enc.starts_with(ENC_PREFIX));
        assert!(!enc.contains("hunter2"));
        assert_eq!(decrypt_value(secret, &enc).as_deref(), Some("hunter2"));
        // Wrong secret cannot decrypt.
        assert!(decrypt_value("other", &enc).is_none());
        // Plain values pass through unwrap unchanged.
        assert_eq!(unwrap_stored(secret, "plain").unwrap(), "plain");
        assert!(unwrap_stored("other", &enc).is_err());
        assert!(unwrap_stored(secret, "enc:v1:invalid").is_err());
        assert!(unwrap_stored(secret, "enc:v2:unknown").is_err());
        assert!(unwrap_stored(secret, &format!("enc:v1:{}界", "a".repeat(23))).is_err());
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(hex_encode(&[0xde, 0xad, 0xbe, 0xef]), "deadbeef");
        assert_eq!(
            hex_decode("deadbeef").unwrap(),
            vec![0xde, 0xad, 0xbe, 0xef]
        );
        assert!(hex_decode("abc").is_none());
        let nonce = random_nonce();
        assert_eq!(nonce.len(), 12);
    }

    #[test]
    fn effective_layering() {
        let s = schema();
        let mut file = BTreeMap::new();
        file.insert("site_title".into(), ConfigValue::Str("File".into()));
        let mut db = BTreeMap::new();
        db.insert("site_title".into(), ConfigValue::Str("DB".into()));
        db.insert("posts_per_page".into(), ConfigValue::Int(20));
        let mut env = BTreeMap::new();
        env.insert("site_title".into(), ConfigValue::Str("Env".into()));

        let eff = compute_effective(&s, &file, &db, &env);
        assert_eq!(eff.get("site_title").unwrap().to_cmp_string(), "Env"); // env wins
        assert_eq!(eff.get("posts_per_page"), Some(&ConfigValue::Int(20))); // db
        assert!(!eff.contains_key("api_key")); // no value anywhere → absent

        let eff = compute_effective(&s, &file, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(eff.get("site_title").unwrap().to_cmp_string(), "File"); // file over schema default
        let eff = compute_effective(&s, &BTreeMap::new(), &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(
            eff.get("site_title").unwrap().to_cmp_string(),
            "Polaris Blog"
        ); // schema default
    }

    #[test]
    fn file_defaults_parse_against_schema() {
        let s = schema();
        let raw = r#"
site_title = "From file"
posts_per_page = 15
unknown_key = "ignored"
"#;
        let defaults = parse_file_defaults(&s, raw);
        assert_eq!(
            defaults.get("site_title").unwrap().to_cmp_string(),
            "From file"
        );
        assert_eq!(defaults.get("posts_per_page"), Some(&ConfigValue::Int(15)));
        assert!(!defaults.contains_key("unknown_key"));
    }

    #[tokio::test]
    async fn save_validates_isolates_and_persists() {
        let db = test_db().await;
        let mgr = ConfigManager::new(db.clone(), "s3cret".into());
        let ns = "theme.default";
        mgr.load_namespace(ns, schema(), BTreeMap::new())
            .await
            .unwrap();

        // Unknown keys are rejected (namespace isolation): neither core.* nor
        // other plugins' keys can be written through this namespace.
        for bad_key in ["core.secret", "plugin.other.key", "theme.other.key"] {
            let mut bad = HashMap::new();
            bad.insert(bad_key.to_string(), "x".to_string());
            assert!(
                mgr.save(ns, &bad, config_schema::Permission::Admin)
                    .await
                    .is_err()
            );
        }

        // Out-of-range value → rejected.
        let mut bad = HashMap::new();
        bad.insert("posts_per_page".to_string(), "500".to_string());
        let err = mgr
            .save(ns, &bad, config_schema::Permission::Admin)
            .await
            .unwrap_err();
        assert!(err.contains("Posts per page"));

        // Permission gate: api_key is admin-only.
        let mut bad = HashMap::new();
        bad.insert("api_key".to_string(), "k".to_string());
        assert!(
            mgr.save(ns, &bad, config_schema::Permission::Editor)
                .await
                .is_err()
        );

        // Valid save persists, reports a change, and reads back.
        let mut input = HashMap::new();
        input.insert("site_title".to_string(), "My Blog".to_string());
        input.insert("api_key".to_string(), "hunter2".to_string());
        let out = mgr
            .save(ns, &input, config_schema::Permission::Admin)
            .await
            .unwrap();
        assert_eq!(out.changes.len(), 2);
        assert!(!out.restart_required);
        assert_eq!(mgr.get_string(ns, "site_title").as_deref(), Some("My Blog"));
        assert_eq!(mgr.get_string(ns, "api_key").as_deref(), Some("hunter2"));

        // Secrets are encrypted at rest.
        let row = db
            .fetch_optional(
                "SELECT value FROM settings WHERE name = ?",
                &[Bind::S("theme.default.api_key".into())],
            )
            .await
            .unwrap()
            .unwrap();
        let stored: String = row.try_get("value").unwrap();
        assert!(stored.starts_with(ENC_PREFIX));
        assert!(!stored.contains("hunter2"));

        // Empty non-secret input removes the override (falls back to default).
        let mut input = HashMap::new();
        input.insert("site_title".to_string(), String::new());
        let out = mgr
            .save(ns, &input, config_schema::Permission::Admin)
            .await
            .unwrap();
        assert_eq!(out.changes.len(), 1);
        assert_eq!(
            mgr.get_string(ns, "site_title").as_deref(),
            Some("Polaris Blog")
        );

        // Empty secret input keeps the current value.
        let mut input = HashMap::new();
        input.insert("api_key".to_string(), String::new());
        mgr.save(ns, &input, config_schema::Permission::Admin)
            .await
            .unwrap();
        assert_eq!(mgr.get_string(ns, "api_key").as_deref(), Some("hunter2"));

        // A fresh ConfigManager reloads the persisted state from the database.
        let mgr2 = ConfigManager::new(db, "s3cret".into());
        mgr2.load_namespace(ns, schema(), BTreeMap::new())
            .await
            .unwrap();
        assert_eq!(mgr2.get_string(ns, "api_key").as_deref(), Some("hunter2"));
    }

    #[tokio::test]
    async fn secrets_are_masked_in_values_json() {
        let db = test_db().await;
        let mgr = ConfigManager::new(db, "s3cret".into());
        let ns = "plugin.analytics";
        mgr.load_namespace(ns, schema(), BTreeMap::new())
            .await
            .unwrap();
        let mut input = HashMap::new();
        input.insert("api_key".to_string(), "hunter2".to_string());
        input.insert("site_title".to_string(), "X".to_string());
        mgr.save(ns, &input, config_schema::Permission::Admin)
            .await
            .unwrap();

        let json = mgr.values_json(ns, false);
        assert_eq!(json["api_key"], serde_json::Value::Null);
        assert_eq!(json["site_title"], "X");
        let json = mgr.values_json(ns, true);
        assert_eq!(json["api_key"], "hunter2");
    }

    #[tokio::test]
    async fn wrong_key_does_not_expose_ciphertext_as_configuration() {
        let db = test_db().await;
        let stored = encrypt_value("original-key", "private-value").unwrap();
        crate::repositories::settings::set(&db, "theme.default.api_key", &stored)
            .await
            .unwrap();
        let manager = ConfigManager::new(db, "wrong-key".into());
        let result = manager
            .load_namespace("theme.default", schema(), BTreeMap::new())
            .await;
        let error = result.err().expect("wrong key must fail");
        assert!(error.contains("cannot decrypt"));
        assert!(!error.contains(&stored));
        assert!(manager.entry("theme.default").is_none());
        assert!(manager.get_string("theme.default", "api_key").is_none());
    }

    #[tokio::test]
    async fn required_check_uses_effective_value() {
        let raw = r#"
[api_key]
type = "password"
label = "API key"
required = true
default = "builtin"
"#;
        let db = test_db().await;
        let mgr = ConfigManager::new(db, "s3cret".into());
        let ns = "plugin.req";
        mgr.load_namespace(
            ns,
            config_schema::parse_schema(raw).unwrap(),
            BTreeMap::new(),
        )
        .await
        .unwrap();
        // Default satisfies required even with no input.
        assert!(
            mgr.save(ns, &HashMap::new(), config_schema::Permission::Admin)
                .await
                .is_ok()
        );

        let raw = r#"
[api_key]
type = "password"
label = "API key"
required = true
"#;
        let mgr2 = ConfigManager::new(test_db().await, "s3cret".into());
        mgr2.load_namespace(
            "plugin.req2",
            config_schema::parse_schema(raw).unwrap(),
            BTreeMap::new(),
        )
        .await
        .unwrap();
        let err = mgr2
            .save(
                "plugin.req2",
                &HashMap::new(),
                config_schema::Permission::Admin,
            )
            .await
            .unwrap_err();
        assert!(err.contains("required"));
    }

    #[test]
    fn env_var_collection_matches_namespaced_keys() {
        // Unit-level check of the segment sanitizer used for env prefixes.
        assert_eq!(env_segment("polaris-default"), "POLARIS_DEFAULT");
        assert_eq!(env_segment("myPlugin"), "MYPLUGIN");
        assert_eq!(ns_kind("theme.default"), "theme");
        assert_eq!(ns_name("theme.default"), "default");
        assert_eq!(theme_ns("default"), "theme.default");
        assert_eq!(plugin_ns("analytics"), "plugin.analytics");
    }

    async fn test_db() -> Db {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(sqlx::any::install_default_drivers);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        std::mem::forget(dir); // keep the file alive for the test
        let cfg = crate::config::DatabaseConfig {
            driver: "sqlite".into(),
            url: path.to_string_lossy().to_string(),
            ..Default::default()
        };
        let db = Db::connect(&cfg).await.unwrap();
        db.execute(
            "CREATE TABLE IF NOT EXISTS settings (name TEXT PRIMARY KEY, value TEXT NOT NULL)",
            &[],
        )
        .await
        .unwrap();
        db
    }
}
