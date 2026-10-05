//! Configuration schema — the contract between themes/plugins and Polaris Core.
//!
//! A theme or plugin ships a declarative `config.schema.toml`; Polaris parses
//! it into a [`SchemaDef`], generates the admin settings UI from it, validates
//! user input against it and exposes effective values to templates and plugin
//! scripts. Theme/plugin developers never touch core source code.
//!
//! ```toml
//! [groups.appearance]
//! label = "Appearance"
//!
//! [accent_color]
//! type = "color"
//! label = "Accent color"
//! default = "#4F46E5"
//! group = "appearance"
//! ```

use std::collections::BTreeMap;

use serde_json::json;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldType {
    String,
    Text,
    Textarea,
    Integer,
    Float,
    Boolean,
    Color,
    Url,
    Email,
    Password,
    Select,
    Multiselect,
    Radio,
    Image,
    File,
    Array,
}

impl FieldType {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "string" => Self::String,
            "text" => Self::Text,
            "textarea" => Self::Textarea,
            "integer" | "int" => Self::Integer,
            "float" | "number" => Self::Float,
            "boolean" | "bool" => Self::Boolean,
            "color" => Self::Color,
            "url" => Self::Url,
            "email" => Self::Email,
            // Secret aliases share the password type (masked + encrypted).
            "password" | "api_key" | "secret" | "token" | "private_key" => Self::Password,
            "select" => Self::Select,
            "multiselect" => Self::Multiselect,
            "radio" => Self::Radio,
            "image" => Self::Image,
            "file" => Self::File,
            "array" => Self::Array,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::String => "string",
            Self::Text => "text",
            Self::Textarea => "textarea",
            Self::Integer => "integer",
            Self::Float => "float",
            Self::Boolean => "boolean",
            Self::Color => "color",
            Self::Url => "url",
            Self::Email => "email",
            Self::Password => "password",
            Self::Select => "select",
            Self::Multiselect => "multiselect",
            Self::Radio => "radio",
            Self::Image => "image",
            Self::File => "file",
            Self::Array => "array",
        }
    }

    /// Secret values are encrypted at rest, masked in the UI and excluded
    /// from template contexts.
    pub fn is_sensitive(self) -> bool {
        matches!(self, Self::Password)
    }

    /// Rough HTML input shape for the generated admin form.
    pub fn input_kind(self) -> &'static str {
        match self {
            Self::String | Self::Url | Self::Email | Self::Image | Self::File => "text",
            Self::Text | Self::Textarea => "textarea",
            Self::Integer | Self::Float => "number",
            Self::Boolean => "checkbox",
            Self::Color => "color",
            Self::Password => "password",
            Self::Select | Self::Radio => "select",
            Self::Multiselect => "multiselect",
            Self::Array => "array",
        }
    }
}

/// Who may modify a field (checked on save). Sensitive fields default to
/// admin-only; everything else defaults to editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    Public,
    Author,
    Editor,
    Admin,
}

impl Permission {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "public" => Self::Public,
            "author" => Self::Author,
            "editor" => Self::Editor,
            "admin" => Self::Admin,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Author => "author",
            Self::Editor => "editor",
            Self::Admin => "admin",
        }
    }
}

/// A single configuration value (the closed set of shapes schemas can declare).
#[derive(Clone, Debug, PartialEq)]
pub enum ConfigValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    /// Array of objects, e.g. social links: `[{label, url}, ...]`.
    Array(Vec<BTreeMap<String, ConfigValue>>),
}

impl ConfigValue {
    pub fn to_json(&self) -> serde_json::Value {
        match self {
            Self::Str(s) => json!(s),
            Self::Int(i) => json!(i),
            Self::Float(f) => json!(f),
            Self::Bool(b) => json!(b),
            Self::Array(items) => json!(
                items
                    .iter()
                    .map(|item| {
                        serde_json::Map::from_iter(
                            item.iter().map(|(k, v)| (k.clone(), v.to_json())),
                        )
                    })
                    .collect::<Vec<_>>()
            ),
        }
    }

    /// Canonical storage form (the `settings.value` column).
    pub fn to_store(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Array(items) => {
                serde_json::to_string(&array_to_json(items)).unwrap_or_else(|_| "[]".into())
            }
        }
    }

    /// Flat string form used for `show_if` comparisons.
    pub fn to_cmp_string(&self) -> String {
        match self {
            Self::Str(s) => s.clone(),
            Self::Int(i) => i.to_string(),
            Self::Float(f) => f.to_string(),
            Self::Bool(b) => b.to_string(),
            Self::Array(items) => serde_json::to_string(&array_to_json(items)).unwrap_or_default(),
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Str(s) => s.is_empty(),
            Self::Int(_) | Self::Float(_) | Self::Bool(_) => false,
            Self::Array(items) => items.is_empty(),
        }
    }
}

fn array_to_json(items: &[BTreeMap<String, ConfigValue>]) -> serde_json::Value {
    json!(
        items
            .iter()
            .map(|item| serde_json::Map::from_iter(
                item.iter().map(|(k, v)| (k.clone(), v.to_json()))
            ))
            .collect::<Vec<_>>()
    )
}

// ---------------------------------------------------------------------------
// Schema definitions
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
pub struct GroupDef {
    pub id: String,
    pub label: String,
}

#[derive(Clone, Debug)]
pub struct FieldDef {
    pub key: String,
    pub ty: FieldType,
    pub label: String,
    pub description: String,
    pub default: Option<ConfigValue>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub required: bool,
    /// `(value, label)` pairs for select/multiselect/radio.
    pub options: Vec<(String, String)>,
    pub group: String,
    pub show_if: Option<String>,
    pub permission: Permission,
    /// Changing this field cannot be hot-applied; warn the admin.
    pub restart: bool,
    /// For `array` fields: the object's sub-fields.
    pub item: Vec<FieldDef>,
}

impl FieldDef {
    pub fn sensitive(&self) -> bool {
        self.ty.is_sensitive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct SchemaDef {
    /// Declared order preserved (requires toml `preserve_order`).
    pub groups: Vec<GroupDef>,
    pub fields: Vec<FieldDef>,
}

impl SchemaDef {
    pub fn field(&self, key: &str) -> Option<&FieldDef> {
        self.fields.iter().find(|f| f.key == key)
    }

    /// Groups in display order: declared groups first, then any group id
    /// referenced by a field but never declared, then ungrouped fields.
    pub fn display_groups(&self) -> Vec<(String, String, Vec<&FieldDef>)> {
        let mut order: Vec<(String, String)> = self
            .groups
            .iter()
            .map(|g| (g.id.clone(), g.label.clone()))
            .collect();
        for f in &self.fields {
            if !f.group.is_empty() && !order.iter().any(|(id, _)| id == &f.group) {
                order.push((f.group.clone(), f.group.clone()));
            }
        }
        let has_ungrouped = self.fields.iter().any(|f| f.group.is_empty());
        if has_ungrouped {
            order.push(("".into(), "General".into()));
        }
        order
            .into_iter()
            .map(|(id, label)| {
                let fields: Vec<&FieldDef> = self.fields.iter().filter(|f| f.group == id).collect();
                (id, label, fields)
            })
            .filter(|(_, _, fields)| !fields.is_empty())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// TOML parsing
// ---------------------------------------------------------------------------

const KNOWN_KEYS: &[&str] = &[
    "type",
    "label",
    "description",
    "default",
    "min",
    "max",
    "required",
    "options",
    "group",
    "show_if",
    "permission",
    "restart",
    "item",
];

/// Parse a `config.schema.toml` document. Unknown field keys are rejected so
/// typos fail loudly at load time instead of silently ignoring validation.
pub fn parse_schema(raw: &str) -> Result<SchemaDef, String> {
    let root: toml::Value = toml::from_str(raw).map_err(|e| format!("invalid TOML: {e}"))?;
    let table = root.as_table().ok_or("schema root must be a table")?;

    let mut schema = SchemaDef::default();
    for (key, value) in table {
        match key.as_str() {
            "groups" => {
                let t = value
                    .as_table()
                    .ok_or("`groups` must be a table of tables")?;
                for (gid, gval) in t {
                    let label = gval
                        .as_table()
                        .and_then(|t| t.get("label"))
                        .and_then(toml::Value::as_str)
                        .unwrap_or(gid)
                        .to_string();
                    schema.groups.push(GroupDef {
                        id: gid.clone(),
                        label,
                    });
                }
            }
            other => {
                let t = value
                    .as_table()
                    .ok_or_else(|| format!("`{other}` must be a table (field definition)"))?;
                let field = parse_field(other, t)?;
                schema.fields.push(field);
            }
        }
    }
    Ok(schema)
}

fn parse_field(key: &str, t: &toml::value::Table) -> Result<FieldDef, String> {
    // Validate keys first: catch typos like `defualt`.
    for k in t.keys() {
        if k != "item" && !KNOWN_KEYS.contains(&k.as_str()) {
            return Err(format!("field `{key}`: unknown key `{k}`"));
        }
    }

    let ty_str = t
        .get("type")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| format!("field `{key}`: `type` is required"))?;
    let ty = FieldType::parse(ty_str)
        .ok_or_else(|| format!("field `{key}`: unknown type `{ty_str}`"))?;

    let mut field = FieldDef {
        key: key.to_string(),
        ty,
        label: t
            .get("label")
            .and_then(toml::Value::as_str)
            .unwrap_or(key)
            .to_string(),
        description: t
            .get("description")
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_string(),
        default: t.get("default").map(toml_to_value).transpose()?,
        min: t.get("min").and_then(toml::Value::as_float),
        max: t.get("max").and_then(toml::Value::as_float),
        required: t
            .get("required")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
        options: parse_options(t.get("options"))?,
        group: t
            .get("group")
            .and_then(toml::Value::as_str)
            .unwrap_or("")
            .to_string(),
        show_if: t
            .get("show_if")
            .and_then(toml::Value::as_str)
            .map(str::to_string),
        permission: t
            .get("permission")
            .and_then(toml::Value::as_str)
            .and_then(Permission::parse)
            .unwrap_or(if ty.is_sensitive() {
                Permission::Admin
            } else {
                Permission::Editor
            }),
        restart: t
            .get("restart")
            .and_then(toml::Value::as_bool)
            .unwrap_or(false),
        item: Vec::new(),
    };

    // Numeric min/max may be written as integers.
    if field.min.is_none() {
        field.min = t
            .get("min")
            .and_then(toml::Value::as_integer)
            .map(|i| i as f64);
    }
    if field.max.is_none() {
        field.max = t
            .get("max")
            .and_then(toml::Value::as_integer)
            .map(|i| i as f64);
    }

    if ty == FieldType::Array
        && let Some(item) = t.get("item")
    {
        let it = item
            .as_table()
            .ok_or("`item` must be a table of sub-fields")?;
        for (sub_key, sub_val) in it {
            let st = sub_val
                .as_table()
                .ok_or_else(|| format!("array item `{sub_key}` must be a table"))?;
            let mut f = parse_field(sub_key, st)?;
            // Sub-fields are plain data; strip grouping/permissions.
            f.group = String::new();
            f.permission = Permission::Editor;
            f.show_if = None;
            field.item.push(f);
        }
    }

    // Defaults must themselves be valid (a broken default is a developer bug).
    if let Some(d) = &field.default
        && let Err(e) = validate_value(&field, d)
    {
        return Err(format!("field `{key}`: invalid default: {e}"));
    }
    Ok(field)
}

fn parse_options(v: Option<&toml::Value>) -> Result<Vec<(String, String)>, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    match v {
        toml::Value::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    toml::Value::String(s) => out.push((s.clone(), s.clone())),
                    toml::Value::Table(t) => {
                        let value = t
                            .get("value")
                            .and_then(toml::Value::as_str)
                            .map(str::to_string);
                        let label = t
                            .get("label")
                            .and_then(toml::Value::as_str)
                            .map(str::to_string);
                        match (value, label) {
                            (Some(value), Some(label)) => out.push((value, label)),
                            _ => return Err("option entries need `value` and `label`".into()),
                        }
                    }
                    _ => return Err("options entries must be strings or tables".into()),
                }
            }
            Ok(out)
        }
        _ => Err("`options` must be an array".into()),
    }
}

fn toml_to_value(v: &toml::Value) -> Result<ConfigValue, String> {
    Ok(match v {
        toml::Value::String(s) => ConfigValue::Str(s.clone()),
        toml::Value::Integer(i) => ConfigValue::Int(*i),
        toml::Value::Float(f) => ConfigValue::Float(*f),
        toml::Value::Boolean(b) => ConfigValue::Bool(*b),
        toml::Value::Array(items) => {
            let mut rows = Vec::with_capacity(items.len());
            for item in items {
                let t = item
                    .as_table()
                    .ok_or("array defaults must be tables (objects)")?;
                let mut row = BTreeMap::new();
                for (k, val) in t {
                    row.insert(k.clone(), toml_to_value(val)?);
                }
                rows.push(row);
            }
            ConfigValue::Array(rows)
        }
        _ => return Err("unsupported default value type".into()),
    })
}

/// Load a schema from disk. A missing file is not an error (empty schema);
/// an unreadable or invalid file is.
pub fn load_schema_file(path: &std::path::Path) -> Result<SchemaDef, String> {
    if !path.exists() {
        return Ok(SchemaDef::default());
    }
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    parse_schema(&raw).map_err(|e| format!("{}: {e}", path.display()))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Parse one raw input string into a typed value, validating against the
/// field definition. Empty input returns `None` (meaning "remove override /
/// fall back to default").
pub fn parse_input(field: &FieldDef, raw: &str) -> Result<Option<ConfigValue>, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let v = parse_non_empty(field, raw)?;
    validate_value(field, &v)?;
    Ok(Some(v))
}

fn parse_non_empty(field: &FieldDef, raw: &str) -> Result<ConfigValue, String> {
    let label = &field.label;
    Ok(match field.ty {
        FieldType::Integer => ConfigValue::Int(
            raw.parse::<i64>()
                .map_err(|_| format!("{label}: must be an integer"))?,
        ),
        FieldType::Float => ConfigValue::Float(
            raw.parse::<f64>()
                .map_err(|_| format!("{label}: must be a number"))?,
        ),
        FieldType::Boolean => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "on" | "yes" => ConfigValue::Bool(true),
            "false" | "0" | "off" | "no" => ConfigValue::Bool(false),
            _ => return Err(format!("{label}: must be true or false")),
        },
        FieldType::Array => {
            let rows: Vec<serde_json::Value> = serde_json::from_str(raw)
                .map_err(|_| format!("{label}: must be a JSON array of objects"))?;
            let mut out = Vec::with_capacity(rows.len());
            for row in rows {
                let obj = row
                    .as_object()
                    .ok_or_else(|| format!("{label}: array entries must be objects"))?;
                let mut m = BTreeMap::new();
                for (k, val) in obj {
                    m.insert(k.clone(), json_to_value(val));
                }
                out.push(m);
            }
            ConfigValue::Array(out)
        }
        _ => ConfigValue::Str(raw.to_string()),
    })
}

fn json_to_value(v: &serde_json::Value) -> ConfigValue {
    match v {
        serde_json::Value::Bool(b) => ConfigValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                ConfigValue::Int(i)
            } else {
                ConfigValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => ConfigValue::Str(s.clone()),
        serde_json::Value::Array(items) => {
            // Nested arrays are not schema-able; keep as JSON string rows.
            ConfigValue::Str(serde_json::to_string(items).unwrap_or_default())
        }
        serde_json::Value::Object(o) => {
            let mut m = BTreeMap::new();
            for (k, val) in o {
                m.insert(k.clone(), json_to_value(val));
            }
            ConfigValue::Array(vec![m])
        }
        serde_json::Value::Null => ConfigValue::Str(String::new()),
    }
}

/// Full validation of a typed value against a field definition.
pub fn validate_value(field: &FieldDef, v: &ConfigValue) -> Result<(), String> {
    let label = &field.label;
    match field.ty {
        FieldType::Integer => match v {
            ConfigValue::Int(i) => check_range(field, *i as f64, label),
            _ => Err(format!("{label}: must be an integer")),
        },
        FieldType::Float => match v {
            ConfigValue::Float(f) => check_range(field, *f, label),
            ConfigValue::Int(i) => check_range(field, *i as f64, label),
            _ => Err(format!("{label}: must be a number")),
        },
        FieldType::Boolean => {
            if !matches!(v, ConfigValue::Bool(_)) {
                return Err(format!("{label}: must be a boolean"));
            }
            Ok(())
        }
        FieldType::Color => match v {
            ConfigValue::Str(s) if is_valid_color(s) => Ok(()),
            _ => Err(format!("{label}: must be a hex color like #4F46E5")),
        },
        FieldType::Url => match v {
            ConfigValue::Str(s) if is_valid_url(s) => Ok(()),
            _ => Err(format!("{label}: must be an http(s) URL")),
        },
        FieldType::Image => match v {
            ConfigValue::Str(s) if is_valid_image_url(s) => Ok(()),
            _ => Err(format!(
                "{label}: must be an http(s) URL or a site-relative path"
            )),
        },
        FieldType::Email => match v {
            ConfigValue::Str(s) if is_valid_email(s) => Ok(()),
            _ => Err(format!("{label}: must be an email address")),
        },
        FieldType::Select | FieldType::Radio => match v {
            ConfigValue::Str(s) if field.options.iter().any(|(val, _)| val == s) => Ok(()),
            ConfigValue::Str(s) => Err(format!("{label}: '{s}' is not one of the allowed options")),
            _ => Err(format!("{label}: must be a string")),
        },
        FieldType::Multiselect => match v {
            ConfigValue::Str(s) => {
                for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    if !field.options.iter().any(|(val, _)| val == part) {
                        return Err(format!(
                            "{label}: '{part}' is not one of the allowed options"
                        ));
                    }
                }
                Ok(())
            }
            _ => Err(format!("{label}: must be a string")),
        },
        FieldType::Array => match v {
            ConfigValue::Array(rows) => {
                for row in rows {
                    for sub in &field.item {
                        let value = row.get(&sub.key);
                        if value.is_none_or(ConfigValue::is_empty) {
                            if sub.required {
                                return Err(format!("{label}: {} is required", sub.label));
                            }
                            continue;
                        }
                        if let Some(val) = value {
                            validate_value(sub, val)
                                .map_err(|e| format!("{label}: row entry: {e}"))?;
                        }
                    }
                }
                Ok(())
            }
            _ => Err(format!("{label}: must be an array")),
        },
        // string/text/textarea/password/file: any string passes
        _ => {
            if !matches!(v, ConfigValue::Str(_)) {
                return Err(format!("{label}: must be a string"));
            }
            Ok(())
        }
    }
}

fn check_range(field: &FieldDef, v: f64, label: &str) -> Result<(), String> {
    if let Some(min) = field.min
        && v < min
    {
        return Err(format!("{label}: must be at least {min}"));
    }
    if let Some(max) = field.max
        && v > max
    {
        return Err(format!("{label}: must be at most {max}"));
    }
    Ok(())
}

fn is_valid_color(s: &str) -> bool {
    let hex = s.strip_prefix('#').unwrap_or("");
    let ok = hex.len() == 3 || hex.len() == 6 || hex.len() == 8;
    ok && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_valid_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("http://") || s.starts_with("https://"))
        && !s.contains(char::is_whitespace)
        && s.len() > 8
}

pub(crate) fn is_valid_image_url(s: &str) -> bool {
    let s = s.trim();
    if s.contains(|c: char| c.is_whitespace() || c.is_control() || c == '\\') {
        return false;
    }
    if s.starts_with('/') && !s.starts_with("//") {
        return true;
    }
    is_valid_url(s)
}

fn is_valid_email(s: &str) -> bool {
    let Some((local, domain)) = s.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.contains(char::is_whitespace)
}

// ---------------------------------------------------------------------------
// show_if expressions
// ---------------------------------------------------------------------------

/// Minimal condition expression: `key == value`, `key != value`, joined by
/// `&&`. Values compare against the string form of the current value.
#[derive(Clone, Debug)]
pub struct ShowIf {
    conditions: Vec<(String, String, bool)>, // (key, expected, negate)
}

impl ShowIf {
    pub fn parse(expr: &str) -> Result<Self, String> {
        let mut conditions = Vec::new();
        for part in expr.split("&&") {
            let part = part.trim();
            if part.is_empty() {
                return Err("empty condition".into());
            }
            let (key, expected, negate) = if let Some((k, v)) = part.split_once("==") {
                (k.trim(), v.trim(), false)
            } else if let Some((k, v)) = part.split_once("!=") {
                (k.trim(), v.trim(), true)
            } else {
                return Err(format!("`{part}` must look like `key == value`"));
            };
            if key.is_empty() || expected.is_empty() {
                return Err(format!("`{part}` has an empty key or value"));
            }
            conditions.push((
                key.to_string(),
                expected.trim_matches('"').to_string(),
                negate,
            ));
        }
        Ok(Self { conditions })
    }

    pub fn eval(&self, values: &BTreeMap<String, ConfigValue>) -> bool {
        self.conditions.iter().all(|(key, expected, negate)| {
            let current = values
                .get(key)
                .map(ConfigValue::to_cmp_string)
                .unwrap_or_default();
            (current == *expected) != *negate
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const THEME_SCHEMA: &str = r##"
[groups.general]
label = "General"

[groups.appearance]
label = "Appearance"

[site_title]
type = "string"
label = "Site title"
default = "Polaris Blog"
group = "general"

[accent_color]
type = "color"
label = "Accent color"
default = "#4F46E5"
group = "appearance"

[dark_mode]
type = "boolean"
label = "Dark mode"
default = true
group = "appearance"

[posts_per_page]
type = "integer"
label = "Posts per page"
default = 10
min = 1
max = 50
group = "general"

[layout]
type = "select"
label = "Layout"
default = "default"
group = "appearance"
options = [
    { value = "default", label = "Default" },
    { value = "wide", label = "Wide" },
]

[api_key]
type = "password"
label = "API key"
required = true

[social_links]
type = "array"

[social_links.item.label]
type = "string"

[social_links.item.url]
type = "url"

[tracking_id]
type = "string"
label = "Tracking ID"
show_if = "dark_mode == true"
"##;

    fn schema() -> SchemaDef {
        parse_schema(THEME_SCHEMA).unwrap()
    }

    #[test]
    fn parses_groups_fields_and_defaults() {
        let s = schema();
        assert_eq!(s.groups.len(), 2);
        assert_eq!(s.groups[0].label, "General");
        assert_eq!(s.fields.len(), 8);
        assert_eq!(s.field("accent_color").unwrap().group, "appearance");
        assert_eq!(
            s.field("posts_per_page").unwrap().default,
            Some(ConfigValue::Int(10))
        );
        assert_eq!(s.field("layout").unwrap().options.len(), 2);
    }

    #[test]
    fn unknown_field_key_is_rejected() {
        let raw = "[x]\ntype = \"string\"\nlabell = \"typo\"\n";
        assert!(parse_schema(raw).is_err());
    }

    #[test]
    fn sensitive_defaults_to_admin_permission() {
        let s = schema();
        assert_eq!(s.field("api_key").unwrap().permission, Permission::Admin);
        assert_eq!(
            s.field("site_title").unwrap().permission,
            Permission::Editor
        );
        assert!(s.field("api_key").unwrap().sensitive());
    }

    #[test]
    fn validates_ranges_types_and_formats() {
        let s = schema();
        let per_page = s.field("posts_per_page").unwrap();
        assert!(parse_input(per_page, "500").is_err());
        assert!(parse_input(per_page, "abc").is_err());
        assert!(matches!(
            parse_input(per_page, "10").unwrap(),
            Some(ConfigValue::Int(10))
        ));

        let color = s.field("accent_color").unwrap();
        assert!(parse_input(color, "#GGGGGG").is_err());
        assert!(parse_input(color, "4F46E5").is_err());
        assert!(matches!(
            parse_input(color, "#4F46E5").unwrap(),
            Some(ConfigValue::Str(_))
        ));
        assert!(parse_input(color, "#FFF").is_ok());

        let layout = s.field("layout").unwrap();
        assert!(parse_input(layout, "compact").is_err());
        assert!(parse_input(layout, "wide").is_ok());
    }

    #[test]
    fn empty_input_means_remove_override() {
        let s = schema();
        let f = s.field("site_title").unwrap();
        assert!(parse_input(f, "").unwrap().is_none());
    }

    #[test]
    fn array_validates_sub_fields() {
        let s = schema();
        let f = s.field("social_links").unwrap();
        assert_eq!(f.item.len(), 2);
        let ok = r#"[{"label":"GitHub","url":"https://github.com"}]"#;
        assert!(parse_input(f, ok).is_ok());
        let bad = r#"[{"label":"X","url":"notaurl"}]"#;
        assert!(parse_input(f, bad).is_err());
        // Array values round-trip through the store form.
        let v = parse_input(f, ok).unwrap().unwrap();
        let back = parse_input(f, &v.to_store()).unwrap().unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn show_if_expressions() {
        let expr = ShowIf::parse("enabled == true && layout != compact").unwrap();
        let mut values = BTreeMap::new();
        values.insert("enabled".into(), ConfigValue::Bool(true));
        values.insert("layout".into(), ConfigValue::Str("wide".into()));
        assert!(expr.eval(&values));
        values.insert("layout".into(), ConfigValue::Str("compact".into()));
        assert!(!expr.eval(&values));
        assert!(ShowIf::parse("bogus").is_err());
        assert!(ShowIf::parse("a ==").is_err());
    }

    #[test]
    fn display_groups_orders_and_fills_undeclared() {
        let raw = r#"
[b]
type = "string"
group = "second"
[a]
type = "string"
group = "second"
[z]
type = "string"
"#;
        let s = parse_schema(raw).unwrap();
        let groups = s.display_groups();
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].1, "second");
        assert_eq!(groups[0].2.len(), 2);
        assert_eq!(groups[1].1, "General");
    }

    #[test]
    fn url_and_email_validation() {
        let f = |ty: &str| {
            parse_schema(&format!("[x]\ntype = \"{ty}\"\n"))
                .unwrap()
                .field("x")
                .unwrap()
                .clone()
        };
        assert!(parse_input(&f("url"), "https://example.com").is_ok());
        assert!(parse_input(&f("url"), "ftp://example.com").is_err());
        assert!(parse_input(&f("email"), "a@b.com").is_ok());
        assert!(parse_input(&f("email"), "a@b").is_err());
        assert!(parse_input(&f("email"), "a b@c.com").is_err());
    }
}
