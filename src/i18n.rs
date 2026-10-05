//! Minimal i18n: embedded language packs exposed to templates as `t()`.
//!
//! Packs are flat key/value JSON embedded in the binary. The site locale is
//! a global runtime setting (`site.locale`, edited in Admin → Settings);
//! template rendering calls `{{ t(key="comments.title") }}`, and extra named
//! arguments are substituted into `{name}` placeholders:
//!
//! ```text
//! {{ t(key="pager.page_of", current=1, pages=3) }}
//! ```
//!
//! Missing keys fall back to English, then to the key itself, so templates
//! never render empty for a typo'd key. Values that intentionally contain
//! markup (suffix `_html`) must be rendered with `| safe` — packs are
//! trusted, built-in resources and never interpolate user data.

use std::collections::HashMap;
use std::sync::{OnceLock, RwLock};

use serde_json::Value;
use tera::Function;

const EN: &str = include_str!("i18n/en.json");
const ZH_CN: &str = include_str!("i18n/zh-CN.json");

/// Locales with a bundled pack; anything else falls back to English.
pub const SUPPORTED: [&str; 2] = ["en", "zh-CN"];
pub const DEFAULT_LOCALE: &str = "en";

static LOCALE: RwLock<String> = RwLock::new(String::new());

fn packs() -> &'static HashMap<&'static str, HashMap<String, String>> {
    static PACKS: OnceLock<HashMap<&'static str, HashMap<String, String>>> = OnceLock::new();
    PACKS.get_or_init(|| {
        let mut map = HashMap::new();
        map.insert(
            "en",
            serde_json::from_str(EN).expect("embedded en pack must parse"),
        );
        map.insert(
            "zh-CN",
            serde_json::from_str(ZH_CN).expect("embedded zh-CN pack must parse"),
        );
        map
    })
}

/// Clamp a raw locale to a supported one (unknown → default).
pub fn normalize(locale: &str) -> String {
    let trimmed = locale.trim();
    if SUPPORTED.contains(&trimmed) {
        trimmed.to_owned()
    } else {
        DEFAULT_LOCALE.to_owned()
    }
}

/// Apply the startup locale from the settings store.
pub fn init(locale: Option<&str>) {
    set_locale(locale.unwrap_or(DEFAULT_LOCALE));
}

pub fn set_locale(locale: &str) {
    *crate::utils::lock::write(&LOCALE) = normalize(locale);
}

pub fn locale() -> String {
    crate::utils::lock::read(&LOCALE).clone()
}

/// Look up `key` in `locale`, falling back to English, then to the key.
pub fn translate(locale: &str, key: &str) -> String {
    let packs = packs();
    if let Some(value) = packs.get(locale).and_then(|p| p.get(key)) {
        return value.clone();
    }
    if let Some(value) = packs.get(DEFAULT_LOCALE).and_then(|p| p.get(key)) {
        return value.clone();
    }
    key.to_owned()
}

/// Rust-side counterpart of the template `t()`: translate `key` in the
/// current site locale and substitute `{name}` placeholders from `args`.
///
/// Used for messages that carry runtime values (flash messages, reports);
/// static UI copy goes through [`tr_or`] instead, with the English sentence
/// itself as the key (gettext style).
pub fn tr(key: &str, args: &[(&str, &str)]) -> String {
    substitute(&translate(&locale(), key), args)
}

/// Replace `{name}` placeholders in `value` with `args` entries.
fn substitute(value: &str, args: &[(&str, &str)]) -> String {
    let mut out = value.to_string();
    for (name, val) in args {
        out = out.replace(&format!("{{{name}}}"), val);
    }
    out
}

/// Translate a static UI string in the current site locale. The string is
/// looked up as a pack key; anything without a translation (including
/// already-translated or dynamic text) passes through unchanged, so wrapping
/// a message twice is harmless and the English source stays the fallback.
pub fn tr_or(s: &str) -> String {
    translate(&locale(), s)
}

/// Register the `t()` Tera function on a template engine instance.
pub fn register(tera: &mut tera::Tera) {
    tera.register_function("t", TFunction);
}

struct TFunction;

impl Function for TFunction {
    fn call(&self, args: &HashMap<String, Value>) -> tera::Result<Value> {
        let key = args
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| tera::Error::msg("t() expects a `key` string argument"))?;
        let locale = args
            .get("locale")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .unwrap_or_else(locale);
        let mut out = translate(&locale, key);
        for (name, value) in args {
            if name == "key" || name == "locale" {
                continue;
            }
            let placeholder = format!("{{{name}}}");
            if out.contains(&placeholder) {
                let rendered = match value {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                out = out.replace(&placeholder, &rendered);
            }
        }
        Ok(Value::String(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn falls_back_to_english_then_key() {
        assert_eq!(translate("zh-CN", "nav.home"), "首页");
        // A key that only exists in English still resolves for other locales.
        assert_eq!(translate("zh-CN", "missing-key"), "missing-key");
        assert_eq!(translate("fr", "nav.home"), "Home");
    }

    #[test]
    fn normalizes_unknown_locales() {
        assert_eq!(normalize(" zh-CN "), "zh-CN");
        assert_eq!(normalize("fr-FR"), "en");
    }

    #[test]
    fn tr_substitutes_placeholders_and_tr_or_passes_through() {
        // Sentence keys (gettext style): translated for zh-CN, identity for en.
        assert_eq!(translate("zh-CN", "Post created."), "文章已创建。");
        assert_eq!(translate("en", "Post created."), "Post created.");
        // Unknown strings pass through unchanged (idempotent).
        assert_eq!(translate("zh-CN", "no key for this"), "no key for this");

        // Placeholder substitution with dynamic values.
        assert_eq!(substitute("Slide {n}", &[("n", "7")]), "Slide 7");
        assert_eq!(substitute("第 {n} 张", &[("n", "7")]), "第 7 张");
        assert_eq!(
            substitute(
                "Deleted {n}; {errors}",
                &[("n", "3"), ("errors", "#5: forbidden")],
            ),
            "Deleted 3; #5: forbidden"
        );
    }
}
