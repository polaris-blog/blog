// Polaris example plugin (Rhai).
//
// Demonstrates the full plugin API surface:
//   - lifecycle:  init(config)
//   - hooks:      before_post_create / before_post_update  (map -> map, may modify)
//   - events:     after_post_create / after_comment_create (fire and forget)
//   - markdown:   markdown_before / markdown_after         (string -> string)
//   - navigation: nav() -> [{label, url}]
//   - routes:     /plugins/example/archive      (public)
//   - admin:      /admin/plugins/example/status (session required)
//   - filters:    the `star` Tera filter
//   - config:    config_get / config_get_bool / config_get_int / config_has
//                (read-only, own namespace only) + on_config_changed(event)
//
// Sandboxing: only `log`, `now`, `cache_*` and `config_get*` host functions
// are available. There is no filesystem, network or OS access from script
// code, and no way to write any configuration.

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

fn init(config) {
    // `config` is a snapshot of the effective values at load time; the
    // config_get*() host functions always read the current values, so a save
    // in the admin UI is picked up without reloading the plugin.
    let tagline = config_get("tagline");
    log("info", "example plugin initialized (tagline: " + tagline + ")");
}

// ---------------------------------------------------------------------------
// Hooks (before_*) — receive the payload map, return it (modified or not)
// ---------------------------------------------------------------------------

fn before_post_create(payload) {
    log("info", "before_post_create: " + payload.title);
    // Example modification: titles starting with "WIP " are forced to drafts.
    if payload.title.starts_with("WIP ") {
        payload.status = "draft";
    }
    payload
}

fn before_post_update(payload) {
    log("info", "before_post_update: post #" + payload.id);
    payload
}

// ---------------------------------------------------------------------------
// Events (after_*) — fire and forget
// ---------------------------------------------------------------------------

fn after_post_create(post) {
    log("info", "event PostCreated: " + post.title + " (" + post.status + ")");
}

fn after_post_update(post) {
    log("info", "event PostUpdated: " + post.title);
}

fn after_post_delete(post) {
    log("info", "event PostDeleted: #" + post.id);
}

fn after_comment_create(comment) {
    log("info", "event CommentCreated by " + comment.author);
}

// Fired by Polaris whenever this plugin's configuration is saved in the
// admin UI (one event per changed key).
fn on_config_changed(event) {
    log("info", "config changed: " + event.namespace + "." + event.key
        + " '" + event.old_value + "' -> '" + event.new_value + "'");
    if event.key == "tagline" {
        log("info", "tagline is now: " + config_get("tagline"));
    }
}

// ---------------------------------------------------------------------------
// Markdown transforms (string -> string)
// ---------------------------------------------------------------------------

fn markdown_before(md) {
    md
}

fn markdown_after(html) {
    // Configuration is read live: toggling "Append signature" in the admin
    // UI applies to the next rendered page, no reload needed.
    if config_get_bool("append_signature") {
        html + "\n<p class=\"plugin-signature\">✦ rendered by the example plugin</p>"
    } else {
        html
    }
}

// ---------------------------------------------------------------------------
// Navigation
// ---------------------------------------------------------------------------

fn nav() {
    if config_get_bool("show_archive_nav") {
        [
            #{label: "Archive", url: "/plugins/example/archive"}
        ]
    } else {
        []
    }
}

// ---------------------------------------------------------------------------
// Routes — receive a ctx map (path + query params; `user` on admin routes)
// and return a string (HTML) or a map {status, body, content_type}.
// ---------------------------------------------------------------------------

fn route_archive(ctx) {
    let body = "<h1>Archive</h1>\n"
        + "<p>This page is served by the <strong>example</strong> plugin at <code>" + ctx.path + "</code>.</p>\n"
        + "<p>Path: " + ctx.path + "</p>\n"
        + "<p>Registered in <code>plugin.toml</code> under <code>[routes]</code>.</p>";
    #{
        status: 200,
        content_type: "text/html; charset=utf-8",
        body: body,
    }
}

fn route_status(ctx) {
    let who = "anonymous";
    if "user" in ctx {
        who = ctx.user.username + " (" + ctx.user.role + ")";
    }
    let body = "<h1>Example plugin status</h1>\n"
        + "<p>Signed in as: " + who + "</p>\n"
        + "<p>Host functions available: <code>log</code>, <code>now</code> — current epoch is " + now() + ".</p>\n"
        + "<p>This admin page is registered under <code>[admin_routes]</code> and only\n"
        + "reachable with a valid session.</p>";
    #{
        status: 200,
        content_type: "text/html; charset=utf-8",
        body: body,
    }
}

// ---------------------------------------------------------------------------
// Template filter — {{ "hello" | star }}  ->  ✦ hello ✦
// ---------------------------------------------------------------------------

fn filter_star(text) {
    let star = config_get("star_char");
    if star == "" {
        star = "✦";
    }
    star + " " + text + " " + star
}
