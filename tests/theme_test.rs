//! Theme system: the shipped default theme loads and renders; hot switching
//! and path traversal guards work.

mod common;

use std::path::Path;
use std::sync::Arc;

use polaris::plugins::PluginManager;
use polaris::themes::ThemeManager;

fn manager(themes_dir: &Path) -> ThemeManager {
    let plugins = Arc::new(PluginManager::new(
        Path::new("nonexistent-plugins"),
        &[],
        None,
        common::empty_config_manager(),
    ));
    ThemeManager::new(themes_dir, "default", plugins)
}

#[test]
fn shipped_default_theme_loads_and_renders() {
    // Tests run from the crate root, where themes/default lives.
    let mgr = manager(Path::new("themes"));
    let current = mgr.current();
    assert!(!current.is_fallback, "default theme should load from disk");
    assert_eq!(current.dir_name, "default");
    assert!(!current.meta.name.is_empty());

    // Every required template renders with a representative context.
    let ctx = tera_context();
    for tpl in [
        "index.html",
        "post.html",
        "page.html",
        "category.html",
        "tag.html",
        "404.html",
    ] {
        let out = current
            .tera
            .render(tpl, &ctx)
            .unwrap_or_else(|e| panic!("{tpl}: {e}"));
        assert!(!out.is_empty(), "{tpl} rendered empty");
    }

    // The theme is listed as available and active (by directory name).
    let list = mgr.list();
    assert!(
        list.iter()
            .any(|(dir, m, active)| dir == "default" && m.name.contains("Polaris") && *active)
    );
}

#[test]
fn missing_theme_falls_back_to_embedded() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir.path().join("themes"));
    assert!(
        mgr.current().is_fallback,
        "empty themes dir uses the embedded fallback"
    );
    let out = mgr
        .current()
        .tera
        .render("index.html", &tera_context())
        .expect("fallback theme renders");
    assert!(!out.is_empty());
}

#[test]
fn hot_switch_swaps_templates() {
    let dir = tempfile::tempdir().unwrap();
    let themes = dir.path().join("themes");
    // Two minimal themes with distinguishable content.
    for name in ["alpha", "beta"] {
        let tdir = themes.join(name).join("templates");
        std::fs::create_dir_all(&tdir).unwrap();
        std::fs::write(tdir.join("index.html"), format!("THEME={name}")).unwrap();
        std::fs::write(
            themes.join(name).join("theme.toml"),
            format!("name = \"{name}\"\nversion = \"1.0\"\n"),
        )
        .unwrap();
    }

    let mgr = manager(&themes);
    // Neither is "default" → embedded fallback initially.
    assert!(mgr.current().is_fallback);

    // Hot switch to alpha, then beta — no restart, no rebuild.
    mgr.activate("alpha").unwrap();
    assert_eq!(mgr.current_name(), "alpha");
    let out = mgr
        .current()
        .tera
        .render("index.html", &tera_context())
        .unwrap();
    assert_eq!(out, "THEME=alpha");

    mgr.activate("beta").unwrap();
    let out = mgr
        .current()
        .tera
        .render("index.html", &tera_context())
        .unwrap();
    assert_eq!(out, "THEME=beta");

    // Path traversal is rejected.
    assert!(mgr.activate("../evil").is_err());
    assert!(mgr.activate("..\\evil").is_err());
    assert!(mgr.activate("").is_err());
    // Still on beta after the failed attempts.
    assert_eq!(mgr.current_name(), "beta");
}

/// A representative render context mirroring what the HTTP handlers provide.
/// Every template must render against it (theme contract test).
fn tera_context() -> tera::Context {
    let ctx = serde_json::json!({
        "site": { "title": "Test Blog", "description": "A test site" },
        "theme": { "name": "Test", "config": {
            "accent_color": "#2563eb", "dark_mode": true, "show_rss_link": true,
            "typewriter": true,
            "layout": "default", "social_links": [], "footer_text": ""
        } },
        "seo": { "title": "Hello", "description": "A post", "canonical": "http://localhost/posts/hello",
                  "og_type": "article", "og_image": "http://localhost/og.png" },
        "current_year": 2026,
        "nav_pages": [ { "slug": "about", "title": "About" } ],
        "plugin_nav": [],
        "json_ld": "",
        "posts": [ {
            "url": "/posts/hello", "title": "Hello", "date": "2026-01-01", "datetime": "2026-01-01T00:00:00Z",
            "author": "writer", "reading_time": 2, "excerpt": "First post",
            "category": { "slug": "rust", "name": "Rust" }, "featured_image": "/static/hero.png"
        } ],
        "pagination": { "pages": 1, "current": 1, "prev": 0, "next": 0, "has_prev": false, "has_next": false },
        "pagination_base": "",
        "post": {
            "id": 1, "slug": "hello", "title": "Hello", "date": "2026-01-01", "datetime": "2026-01-01T00:00:00Z",
            "author": "writer", "reading_time": 2, "excerpt": "First post", "content_html": "<p>Hi</p>",
            "category": { "slug": "rust", "name": "Rust" }, "tags": [ { "slug": "tag1", "name": "tag1" } ],
            "featured_image": "/static/hero.png"
        },
        "page": { "title": "About", "date": "2026-01-01", "content_html": "<p>About page</p>" },
        "term": { "name": "Rust" },
        "comments": [ { "id": 1, "author": "Visitor", "author_url": "", "date": "2026-01-02",
                        "content": "Nice!", "parent_id": null, "depth": 0 } ],
        "comments_enabled": true,
    });
    tera::Context::from_serialize(&ctx).expect("context serializes")
}
