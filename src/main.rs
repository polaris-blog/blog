//! Polaris CLI — a single binary that does everything:
//!
//! ```text
//! polaris serve               start the blog
//! polaris migrate             apply pending database migrations
//! polaris user create         create a user (interactively or with flags)
//! polaris theme list|install|enable|disable|remove
//! polaris plugin list|install|enable|disable|remove
//! polaris extension verify|logs
//! polaris search status|rebuild
//! ```
//!
//! Configuration: `polaris.toml` < `POLARIS_*` environment < CLI arguments.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};

use polaris::config::Config;
use polaris::db::{Db, migrate};
use polaris::models::Role;
use polaris::state::AppState;

#[derive(Parser)]
#[command(
    name = "polaris",
    version,
    about = "Polaris — a fast, lightweight, secure and extensible blog engine",
    subcommand_required = true,
    arg_required_else_help = true
)]
struct Cli {
    /// Path to the configuration file (default: polaris.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the HTTP server
    Serve {
        /// Override server.host
        #[arg(long)]
        host: Option<String>,
        /// Override server.port
        #[arg(long)]
        port: Option<u16>,
    },
    /// Run pending database migrations
    Migrate,
    /// Manage users
    User {
        #[command(subcommand)]
        command: UserCommands,
    },
    /// Manage themes
    Theme {
        #[command(subcommand)]
        command: ThemeCommands,
    },
    /// Manage plugins
    Plugin {
        #[command(subcommand)]
        command: PluginCommands,
    },
    /// Manage the search index
    Search {
        #[command(subcommand)]
        command: SearchCommands,
    },
    /// Media library maintenance (orphans, integrity)
    Media {
        #[command(subcommand)]
        command: MediaCommands,
    },
    /// Extension management shared by themes and plugins
    Extension {
        #[command(subcommand)]
        command: ExtensionCommands,
    },
    /// Backup & restore
    Backup {
        #[command(subcommand)]
        command: BackupCommands,
    },
}

#[derive(Subcommand)]
enum BackupCommands {
    /// Create a backup (full = database + media + themes + plugins)
    Create {
        /// What to include: full | database | media
        #[arg(long, default_value = "full")]
        kind: String,
    },
    /// List stored backups
    List,
    /// Verify a backup archive's manifest and SHA-256 checksums
    Verify {
        /// Backup file name (in the backup dir) or path to a .zip
        file: String,
    },
    /// Restore a backup archive (requires --yes)
    Restore {
        /// Backup file name (in the backup dir) or path to a .zip
        file: String,
        /// Skip the database part (restore files only)
        #[arg(long, default_value_t = false)]
        no_database: bool,
        /// Skip media files
        #[arg(long, default_value_t = false)]
        no_media: bool,
        /// Skip themes
        #[arg(long, default_value_t = false)]
        no_themes: bool,
        /// Skip plugins
        #[arg(long, default_value_t = false)]
        no_plugins: bool,
        /// Confirm the restore (destructive: replaces current data)
        #[arg(long, default_value_t = false)]
        yes: bool,
    },
    /// Delete a stored backup
    Delete {
        /// Backup file name (in the backup dir) or path to a .zip
        file: String,
    },
    /// Prune scheduled backups beyond the retention count and stale staging files
    Cleanup {
        /// How many scheduled backups to keep (default: backup.keep_auto)
        #[arg(long)]
        keep: Option<usize>,
    },
    /// Show / update the automatic backup schedule
    Schedule {
        /// Enable or disable: on | off (omit to show the current schedule)
        #[arg(long)]
        set: Option<String>,
        /// Interval in hours
        #[arg(long)]
        interval_hours: Option<u64>,
        /// Scheduled backup kind: full | database | media
        #[arg(long)]
        kind: Option<String>,
        /// How many scheduled backups to keep
        #[arg(long)]
        keep: Option<usize>,
    },
}

#[derive(Subcommand)]
enum UserCommands {
    /// Create a new user
    Create {
        username: String,
        /// Email address (optional)
        #[arg(long)]
        email: Option<String>,
        /// Role: author, editor or admin
        #[arg(long, default_value = "author")]
        role: String,
        /// Password (omit to be prompted)
        #[arg(long)]
        password: Option<String>,
    },
}

#[derive(Subcommand)]
enum ThemeCommands {
    /// List installed themes
    List,
    /// Install a theme from a .zip package or a local directory
    Install {
        /// Source .zip package or directory containing theme.toml
        path: PathBuf,
        /// Install under a different name (directory sources only; ZIP ids come from theme.toml)
        #[arg(long)]
        name: Option<String>,
        /// Skip the Polaris version compatibility check
        #[arg(long)]
        force: bool,
        /// Allow installing an older version than the installed one
        #[arg(long)]
        allow_downgrade: bool,
    },
    /// Activate a theme
    Enable { name: String },
    /// Deactivate a theme and switch back to the default theme
    Disable { name: String },
    /// Remove a theme (the active theme cannot be removed)
    Remove { name: String },
}

#[derive(Subcommand)]
enum PluginCommands {
    /// List installed plugins
    List,
    /// Install a plugin from a .zip package or a local directory
    Install {
        /// Source .zip package or directory containing plugin.toml
        path: PathBuf,
        /// Install under a different name (directory sources only; ZIP ids come from plugin.toml)
        #[arg(long)]
        name: Option<String>,
        /// Skip the Polaris version compatibility check
        #[arg(long)]
        force: bool,
        /// Allow installing an older version than the installed one
        #[arg(long)]
        allow_downgrade: bool,
    },
    /// Enable a plugin
    Enable { name: String },
    /// Disable a plugin
    Disable { name: String },
    /// Remove a plugin (database tables are kept unless --remove-data)
    Remove {
        name: String,
        /// Also drop the tables the plugin created
        #[arg(long)]
        remove_data: bool,
    },
}

#[derive(Subcommand)]
enum ExtensionCommands {
    /// Verify every installed extension (manifest, registry, entry files)
    Verify,
    /// Show recent extension install/update/remove activity
    Logs,
}

#[derive(Subcommand)]
enum SearchCommands {
    /// Show search provider and index status
    Status,
    /// Drop and rebuild the search index from the database
    Rebuild {
        /// Override [search] provider for this run (e.g. database, sqlite)
        #[arg(long)]
        provider: Option<String>,
    },
}

#[derive(Subcommand)]
enum MediaCommands {
    /// Detect orphan storage files and orphan database records
    Orphan,
    /// Delete orphan storage files (dry run unless --apply)
    Cleanup {
        /// Actually delete the files
        #[arg(long)]
        apply: bool,
    },
    /// Verify database metadata against stored objects
    Verify {
        /// Also recompute and compare SHA-256 hashes (slower)
        #[arg(long)]
        deep: bool,
    },
}

fn main() {
    let cli = Cli::parse();
    let cfg_path = cli
        .config
        .clone()
        .or_else(|| Some(polaris::config::default_config_path()));
    let result = match cli.command {
        Commands::Serve { host, port } => run(cfg_path, |p| serve_cmd(p, host, port)),
        Commands::Migrate => run(cfg_path, migrate_cmd),
        Commands::User { command } => run(cfg_path, |p| user_cmd(p, command)),
        Commands::Theme { command } => run(cfg_path, |p| theme_cmd(p, command)),
        Commands::Plugin { command } => run(cfg_path, |p| plugin_cmd(p, command)),
        Commands::Search { command } => run(cfg_path, |p| search_cmd(p, command)),
        Commands::Media { command } => run(cfg_path, |p| media_cmd(p, command)),
        Commands::Extension { command } => run(cfg_path, |p| extension_cmd(p, command)),
        Commands::Backup { command } => run(cfg_path, |p| backup_cmd(p, command)),
    };
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run<F, Fut>(cfg_path: Option<PathBuf>, f: F) -> anyhow::Result<()>
where
    F: FnOnce(Option<PathBuf>) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    // Simple blocking wrapper: the CLI is short-lived, one runtime is enough.
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(f(cfg_path))
}

fn load_config(path: Option<&Path>, overrides: HashMap<String, String>) -> anyhow::Result<Config> {
    let mut ov = Config::env_overrides();
    ov.extend(overrides);
    Config::load(path, &ov)
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("polaris=info,sqlx=warn"));
    tracing_subscriber::fmt().with_env_filter(filter).init();
}

// ---------------------------------------------------------------------------
// serve
// ---------------------------------------------------------------------------

async fn serve_cmd(
    cfg_path: Option<PathBuf>,
    host: Option<String>,
    port: Option<u16>,
) -> anyhow::Result<()> {
    init_tracing();
    let mut overrides = HashMap::new();
    if let Some(h) = host {
        overrides.insert("server.host".to_string(), h);
    }
    if let Some(p) = port {
        overrides.insert("server.port".to_string(), p.to_string());
    }
    let mut cfg = load_config(cfg_path.as_deref(), overrides)?;
    cfg.config_path = cfg_path.clone();
    if cfg.security.secret.is_empty() {
        tracing::info!(
            "using the persisted instance encryption key; keep a separate secure copy for disaster recovery"
        );
    }

    let started = std::time::Instant::now();
    let app = AppState::init(cfg.clone()).await?;
    let router = polaris::http::router(app.clone());

    let addr: SocketAddr = format!("{}:{}", cfg.server.host, cfg.server.port).parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        addr = %addr,
        db = cfg.database.driver_normalized(),
        theme = app.theme.current_name(),
        startup_ms = started.elapsed().as_millis() as u64,
        "✦ polaris listening — press Ctrl-C to stop"
    );

    // Background job: promote scheduled posts + run the backup schedule
    // check every minute.
    tokio::spawn({
        let app = app.clone();
        async move {
            let mut tick = tokio::time::interval(Duration::from_secs(60));
            loop {
                tick.tick().await;
                match app.promote_scheduled().await {
                    Ok(n) if n > 0 => tracing::info!(count = n, "scheduled posts published"),
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %e, "scheduled promotion failed"),
                }
                polaris::backup::scheduler::tick(&app).await;
            }
        }
    });

    let scheduler = app.scheduler.start()?;
    let scheduler_stop = scheduler.as_ref().map(|handle| handle.stop_token());
    let server_result = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        if let Some(stop) = scheduler_stop {
            stop.cancel();
        }
    })
    .await;
    if let Some(scheduler) = scheduler {
        scheduler.shutdown().await;
    }
    server_result?;
    tracing::info!("bye");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => {
                s.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

// ---------------------------------------------------------------------------
// migrate
// ---------------------------------------------------------------------------

async fn migrate_cmd(cfg_path: Option<PathBuf>) -> anyhow::Result<()> {
    init_tracing();
    let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
    let db = Db::connect(&cfg.database).await?;
    let applied = migrate::run(&db).await?;
    if applied.is_empty() {
        println!("database is up to date ({})", db.dialect().name());
    } else {
        for v in applied {
            println!("applied migration {v}");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// user
// ---------------------------------------------------------------------------

async fn user_cmd(cfg_path: Option<PathBuf>, command: UserCommands) -> anyhow::Result<()> {
    let UserCommands::Create {
        username,
        email,
        role,
        password,
    } = command;
    init_tracing();
    let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
    let app = AppState::init(cfg).await?;

    let role = Role::parse(&role).ok_or_else(|| {
        anyhow::anyhow!("invalid role '{role}' (expected author, editor or admin)")
    })?;
    let password = match password {
        Some(p) => p,
        None => prompt_password()?,
    };
    let email = email.unwrap_or_default();
    let user =
        polaris::services::users::create_user(&app, &username, &email, &password, role).await?;
    println!(
        "✦ user '{}' created (id {}, role {})",
        user.username,
        user.id,
        user.role.as_str()
    );
    println!(
        "sign in at http://{}:{}/admin",
        app.config.server.host, app.config.server.port
    );
    Ok(())
}

/// Prompt for a password twice on the terminal (no echo hiding — keeps the
/// binary dependency-free; recommend `--password` in scripts with care).
fn prompt_password() -> anyhow::Result<String> {
    print!("password (min 8 chars): ");
    use std::io::Write;
    std::io::stdout().flush()?;
    let mut pw = String::new();
    std::io::stdin().read_line(&mut pw)?;
    let pw = pw.trim().to_string();
    print!("confirm password: ");
    std::io::stdout().flush()?;
    let mut confirm = String::new();
    std::io::stdin().read_line(&mut confirm)?;
    let confirm = confirm.trim().to_string();
    if pw != confirm {
        anyhow::bail!("passwords do not match");
    }
    Ok(pw)
}

// ---------------------------------------------------------------------------
// theme
// ---------------------------------------------------------------------------

async fn theme_cmd(cfg_path: Option<PathBuf>, command: ThemeCommands) -> anyhow::Result<()> {
    match command {
        ThemeCommands::List => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let themes = app.theme.list();
            if themes.is_empty() {
                println!("no themes found in '{}'", app.config.theme.dir);
                return Ok(());
            }
            println!(
                "{:<24} {:<10} {:<8} DESCRIPTION",
                "NAME", "VERSION", "ACTIVE"
            );
            for (dir, meta, active) in themes {
                println!(
                    "{:<24} {:<10} {:<8} {}",
                    dir,
                    meta.version,
                    if active { "yes" } else { "" },
                    meta.description
                );
            }
        }
        ThemeCommands::Install {
            path,
            name,
            force,
            allow_downgrade,
        } => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            if is_zip_package(&path) {
                let app = AppState::init(cfg).await?;
                let out = polaris::services::extensions::install(
                    &app,
                    &path,
                    "cli",
                    force,
                    allow_downgrade,
                )
                .await?;
                if out.kind != polaris::extension::ExtensionKind::Theme {
                    anyhow::bail!(
                        "package is a {} ('{}'), not a theme — use `polaris plugin install`",
                        out.kind.as_str(),
                        out.manifest.id
                    );
                }
                println!("✦ {}", polaris::services::extensions::outcome_summary(&out));
                println!(
                    "  not activated yet — run `polaris theme enable {}`",
                    out.manifest.id
                );
            } else {
                let name = install_dir(&path, &name, &cfg.theme.dir, "theme.toml")?;
                println!("✦ theme '{name}' installed into {}/{}", cfg.theme.dir, name);
            }
        }
        ThemeCommands::Enable { name } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            app.set_active_theme(&name).await?;
            println!("✦ theme '{name}' activated");
        }
        ThemeCommands::Disable { name } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let active = app.theme.current_name();
            if active != name {
                anyhow::bail!("'{name}' is not the active theme (active: '{active}')");
            }
            app.set_active_theme("default").await?;
            println!("✦ theme '{name}' deactivated — 'default' is now active");
        }
        ThemeCommands::Remove { name } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            polaris::services::extensions::uninstall(
                &app,
                polaris::extension::ExtensionKind::Theme,
                &name,
                false,
                "cli",
            )
            .await?;
            println!("✦ theme '{name}' removed");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// plugin
// ---------------------------------------------------------------------------

async fn plugin_cmd(cfg_path: Option<PathBuf>, command: PluginCommands) -> anyhow::Result<()> {
    match command {
        PluginCommands::List => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let plugins = app.plugins.list();
            if plugins.is_empty() {
                println!("no plugins found in '{}'", app.config.plugin.dir);
                return Ok(());
            }
            println!(
                "{:<20} {:<10} {:<8} DESCRIPTION",
                "ID", "VERSION", "ENABLED"
            );
            for (id, meta, enabled) in plugins {
                println!(
                    "{:<20} {:<10} {:<8} {}",
                    id,
                    meta.version,
                    if enabled { "yes" } else { "" },
                    meta.description
                );
            }
        }
        PluginCommands::Install {
            path,
            name,
            force,
            allow_downgrade,
        } => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            if is_zip_package(&path) {
                let app = AppState::init(cfg).await?;
                let out = polaris::services::extensions::install(
                    &app,
                    &path,
                    "cli",
                    force,
                    allow_downgrade,
                )
                .await?;
                if out.kind != polaris::extension::ExtensionKind::Plugin {
                    anyhow::bail!(
                        "package is a {} ('{}'), not a plugin — use `polaris theme install`",
                        out.kind.as_str(),
                        out.manifest.id
                    );
                }
                println!("✦ {}", polaris::services::extensions::outcome_summary(&out));
                println!(
                    "  not enabled yet — run `polaris plugin enable {}`",
                    out.manifest.id
                );
            } else {
                let name = install_dir(&path, &name, &cfg.plugin.dir, "plugin.toml")?;
                println!(
                    "✦ plugin '{name}' installed into {}/{}",
                    cfg.plugin.dir, name
                );
            }
        }
        PluginCommands::Enable { name } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let mut names = app.settings.plugins_enabled();
            if !names.contains(&name) {
                names.push(name.clone());
            }
            app.set_plugins_enabled(&names).await?;
            println!("✦ plugin '{name}' enabled");
        }
        PluginCommands::Disable { name } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let mut names = app.settings.plugins_enabled();
            names.retain(|n| n != &name);
            app.set_plugins_enabled(&names).await?;
            println!("✦ plugin '{name}' disabled");
        }
        PluginCommands::Remove { name, remove_data } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let out = polaris::services::extensions::uninstall(
                &app,
                polaris::extension::ExtensionKind::Plugin,
                &name,
                remove_data,
                "cli",
            )
            .await?;
            let tables = if out.removed_tables.is_empty() {
                String::new()
            } else {
                format!(" (dropped tables: {})", out.removed_tables.join(", "))
            };
            println!("✦ plugin '{}' removed{tables}", out.id);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// extension (shared: verify, logs)
// ---------------------------------------------------------------------------

async fn extension_cmd(
    cfg_path: Option<PathBuf>,
    command: ExtensionCommands,
) -> anyhow::Result<()> {
    match command {
        ExtensionCommands::Verify => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let entries = polaris::services::extensions::verify(&app).await?;
            if entries.is_empty() {
                println!("no extensions installed");
                return Ok(());
            }
            let mut failed = 0;
            for e in entries {
                if e.ok {
                    println!("✓ {:<7} {:<24} ok", e.kind.as_str(), e.id);
                } else {
                    failed += 1;
                    println!(
                        "✗ {:<7} {:<24} {}",
                        e.kind.as_str(),
                        e.id,
                        e.issues.join("; ")
                    );
                }
            }
            if failed > 0 {
                anyhow::bail!("{failed} extension(s) failed verification");
            }
        }
        ExtensionCommands::Logs => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let logs = polaris::services::extensions::logs(&app, None, 50).await?;
            if logs.is_empty() {
                println!("no extension activity recorded yet");
                return Ok(());
            }
            println!(
                "{:<19} {:<12} {:<10} {:<22} {:<10} {:<8} DETAIL",
                "TIME", "ACTOR", "ACTION", "EXTENSION", "VERSION", "RESULT"
            );
            for l in logs {
                println!(
                    "{:<19} {:<12} {:<10} {:<22} {:<10} {:<8} {}",
                    polaris::utils::time::format(l.created_at, "datetime"),
                    l.actor,
                    l.action,
                    format!("{}:{}", l.kind, l.ext_id),
                    l.version,
                    l.result,
                    l.detail
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

async fn search_cmd(cfg_path: Option<PathBuf>, command: SearchCommands) -> anyhow::Result<()> {
    match command {
        SearchCommands::Status => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let s = app.search.status(&app).await?;
            println!("Search Provider: {}", s.provider);
            println!(
                "Status:          {}",
                if s.healthy {
                    "Healthy"
                } else {
                    "Needs rebuild (run `polaris search rebuild`)"
                }
            );
            println!("Indexed Posts:   {}", s.indexed_posts);
            println!("Indexed Pages:   {}", s.indexed_pages);
            println!(
                "Last Rebuild:    {}",
                s.last_rebuild
                    .map(|t| polaris::utils::time::format(t, "datetime"))
                    .unwrap_or_else(|| "never".into())
            );
        }
        SearchCommands::Rebuild { provider } => {
            init_tracing();
            let mut overrides = HashMap::new();
            if let Some(p) = provider {
                overrides.insert("search.provider".to_string(), p);
            }
            let cfg = load_config(cfg_path.as_deref(), overrides)?;
            let app = AppState::init(cfg).await?;
            println!(
                "Building search index (provider: {})…",
                app.search.provider_name()
            );
            let stats = app.search.rebuild(&app, print_progress).await?;
            println!();
            if stats.verified {
                println!(
                    "✦ search index rebuilt: {} posts, {} pages",
                    stats.posts, stats.pages
                );
            } else {
                println!(
                    "✦ search index rebuilt with warnings: {} posts, {} pages — check logs",
                    stats.posts, stats.pages
                );
            }
        }
    }
    Ok(())
}

/// In-place ASCII progress bar: `[████████████░░░░░░] 60%  300/500`.
fn print_progress(done: usize, total: usize) {
    use std::io::Write;
    const WIDTH: usize = 20;
    let frac = if total == 0 {
        1.0
    } else {
        done as f64 / total as f64
    };
    let filled = ((frac * WIDTH as f64) as usize).min(WIDTH);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(WIDTH - filled));
    print!("\r[{bar}] {:>3.0}%  {done}/{total}", frac * 100.0);
    let _ = std::io::stdout().flush();
}

// ---------------------------------------------------------------------------
// media
// ---------------------------------------------------------------------------

async fn media_cmd(cfg_path: Option<PathBuf>, command: MediaCommands) -> anyhow::Result<()> {
    use polaris::services::media;

    let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
    let app = AppState::init(cfg).await?;
    match command {
        MediaCommands::Orphan => {
            let report = media::orphan_report(&app).await?;
            println!("Storage provider: {}", app.media.storage().name());
            if report.records.is_empty() && report.files.is_empty() {
                println!("✦ no orphans — database and storage are in sync");
                return Ok(());
            }
            if !report.records.is_empty() {
                println!(
                    "\nOrphan database records (storage object missing): {}",
                    report.records.len()
                );
                for m in &report.records {
                    println!("  #{} {} → {}", m.id, m.filename, m.storage_key);
                }
                println!("  fix: delete the record or re-upload the file");
            }
            if !report.files.is_empty() {
                let bytes: u64 = report.files.iter().map(|f| f.size).sum();
                println!(
                    "\nOrphan storage files (no database record): {} ({})",
                    report.files.len(),
                    media::human_size(bytes as i64)
                );
                for f in &report.files {
                    println!("  {} ({})", f.key, media::human_size(f.size as i64));
                }
                println!("\nRun `polaris media cleanup --apply` to delete them.");
            }
        }
        MediaCommands::Cleanup { apply } => {
            let deleted = media::cleanup_orphans(&app, apply).await?;
            if deleted.is_empty() {
                println!("✦ nothing to clean — no orphan storage files");
                return Ok(());
            }
            if apply {
                println!("✦ deleted {} orphan file(s):", deleted.len());
                for key in &deleted {
                    println!("  - {key}");
                }
            } else {
                println!(
                    "Found {} orphan file(s). Dry run — nothing deleted.",
                    deleted.len()
                );
                for key in &deleted {
                    println!("  - {key}");
                }
                println!("\nRun with: polaris media cleanup --apply");
            }
        }
        MediaCommands::Verify { deep } => {
            let issues = media::verify(&app, deep).await?;
            if issues.is_empty() {
                println!("✦ all media records match their storage objects");
                return Ok(());
            }
            println!("{} issue(s) found:", issues.len());
            for i in &issues {
                println!(
                    "  WARNING: media #{} ({}) — {}",
                    i.media_id, i.uuid, i.problem
                );
            }
            anyhow::bail!("verification failed with {} issue(s)", issues.len());
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// shared install helper (theme / plugin from a local directory)
// ---------------------------------------------------------------------------

fn is_zip_package(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

fn install_dir(
    src: &Path,
    name: &Option<String>,
    dest_root: &str,
    marker: &str,
) -> anyhow::Result<String> {
    if !src.is_dir() {
        anyhow::bail!("source directory not found: {}", src.display());
    }
    if !src.join(marker).is_file() {
        anyhow::bail!(
            "source is not a valid {} directory (missing {marker})",
            marker
        );
    }
    // The name must be a plain directory name (block path traversal).
    let name = name.clone().unwrap_or_else(|| {
        src.file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string()
    });
    if name.is_empty()
        || name.starts_with('.')
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
    {
        anyhow::bail!("invalid {marker} name '{name}'");
    }
    let dest = Path::new(dest_root).join(&name);
    if dest.exists() {
        anyhow::bail!("'{}' already exists — remove it first", dest.display());
    }
    copy_dir(src, &dest)?;
    Ok(name)
}

fn copy_dir(src: &Path, dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dest.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// backup
// ---------------------------------------------------------------------------

/// Resolve a CLI backup reference: a bare file name inside the backup dir,
/// or any path to an existing .zip.
async fn resolve_backup_path(app: &AppState, file: &str) -> anyhow::Result<std::path::PathBuf> {
    use polaris::backup::storage::valid_backup_name;
    if valid_backup_name(file) {
        return Ok(app.backup().storage_path_of(file)?);
    }
    let p = Path::new(file);
    if p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        return Ok(p.to_path_buf());
    }
    anyhow::bail!("'{file}' is not a backup file name in the backup dir or a path to a .zip")
}

async fn backup_cmd(cfg_path: Option<PathBuf>, command: BackupCommands) -> anyhow::Result<()> {
    use polaris::backup::{BackupKind, BackupSummary};
    use polaris::utils::time;

    let print_row = |b: &BackupSummary| {
        if b.ok {
            println!(
                "{:<44} {:<8} {:<8} {:<19} {:<10} {:<10}",
                b.name,
                b.kind,
                b.dialect,
                time::format(b.created_at, "datetime"),
                b.polaris_version,
                polaris::services::media::human_size(b.zip_bytes as i64),
            );
        } else {
            println!(
                "{:<44} INVALID — {}",
                b.name,
                b.error.clone().unwrap_or_default()
            );
        }
    };

    match command {
        BackupCommands::Create { kind } => {
            init_tracing();
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let kind = BackupKind::parse(kind.trim()).ok_or_else(|| {
                anyhow::anyhow!("unknown kind '{kind}' (expected full | database | media)")
            })?;
            let svc = app.backup();
            let summary = svc.create(kind, "cli").await?;
            println!("✦ backup '{}' created ({})", summary.name, summary.kind);
            println!(
                "  {} file(s), {} on disk",
                summary.counts.files,
                polaris::services::media::human_size(summary.zip_bytes as i64)
            );
        }
        BackupCommands::List => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let list = app.backup().list().await?;
            if list.is_empty() {
                println!("no backups found");
                return Ok(());
            }
            println!(
                "{:<44} {:<8} {:<8} {:<19} {:<10} {:<10}",
                "NAME", "KIND", "DB", "CREATED", "POLARIS", "SIZE"
            );
            for b in &list {
                print_row(b);
            }
        }
        BackupCommands::Verify { file } => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let svc = app.backup();
            let path = resolve_backup_path(&app, &file).await?;
            let manifest = svc.read_manifest(&path)?;
            println!(
                "manifest: format v{}, Polaris {}, dialect {}, kind {}",
                manifest.format_version, manifest.polaris_version, manifest.dialect, manifest.kind
            );
            let report = svc.verify_file(&path)?;
            println!("{}", report.summary());
            if !report.ok() {
                for m in report.mismatched {
                    eprintln!("  corrupted: {m}");
                }
                for m in report.missing {
                    eprintln!("  missing:   {m}");
                }
                anyhow::bail!("verification failed");
            }
        }
        BackupCommands::Restore {
            file,
            no_database,
            no_media,
            no_themes,
            no_plugins,
            yes,
        } => {
            init_tracing();
            if !yes {
                anyhow::bail!("restore replaces current content — re-run with --yes to confirm");
            }
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let path = resolve_backup_path(&app, &file).await?;
            println!("restoring {} …", path.display());
            let report = app
                .backup()
                .restore(
                    &app,
                    &path,
                    polaris::backup::restore::RestoreOptions {
                        database: !no_database,
                        media: !no_media,
                        themes: !no_themes,
                        plugins: !no_plugins,
                    },
                )
                .await?;
            println!("✦ {}", report.summary());
            if let Some(snap) = &report.snapshot {
                println!("  pre-restore snapshot: {snap}");
            }
            for w in &report.warnings {
                println!("  warning: {w}");
            }
        }
        BackupCommands::Delete { file } => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg).await?;
            let svc = app.backup();
            let name = polaris::backup::service::sanitize_requested_name(&file)?;
            if svc.delete(&name).await? {
                println!("✦ backup '{name}' deleted");
            } else {
                anyhow::bail!("backup '{name}' not found");
            }
        }
        BackupCommands::Cleanup { keep } => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let keep = keep.unwrap_or(cfg.backup.keep_auto.max(1));
            let app = AppState::init(cfg).await?;
            let svc = app.backup();
            let pruned = svc.apply_retention(keep).await?;
            let staged = svc.purge_stale_staging().await?;
            println!(
                "✦ pruned {pruned} scheduled backup(s), removed {staged} stale staging file(s)"
            );
        }
        BackupCommands::Schedule {
            set,
            interval_hours,
            kind,
            keep,
        } => {
            let cfg = load_config(cfg_path.as_deref(), HashMap::new())?;
            let app = AppState::init(cfg.clone()).await?;
            if set.is_none() && interval_hours.is_none() && kind.is_none() && keep.is_none() {
                let enabled = app
                    .settings
                    .get_bool("backup.auto.enabled", cfg.backup.auto.enabled);
                let interval = app
                    .settings
                    .get("backup.auto.interval_hours")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(cfg.backup.auto.interval_hours);
                let kind = app
                    .settings
                    .get_str("backup.auto.kind", &cfg.backup.auto.kind);
                let keep = app
                    .settings
                    .get("backup.auto.keep")
                    .and_then(|v| v.parse::<usize>().ok())
                    .unwrap_or(cfg.backup.auto.keep);
                println!(
                    "automatic backups: {} every {interval}h (kind: {kind}, keep {keep})",
                    if enabled { "on" } else { "off" }
                );
                return Ok(());
            }
            if let Some(s) = set {
                let v = match s.trim() {
                    "on" | "true" | "1" => "true",
                    "off" | "false" | "0" => "false",
                    other => anyhow::bail!("invalid value '{other}' (expected on | off)"),
                };
                app.settings.set(&app.db, "backup.auto.enabled", v).await?;
            }
            if let Some(h) = interval_hours {
                if h == 0 {
                    anyhow::bail!("interval must be at least 1 hour");
                }
                app.settings
                    .set(&app.db, "backup.auto.interval_hours", &h.to_string())
                    .await?;
            }
            if let Some(k) = kind {
                if polaris::backup::BackupKind::parse(k.trim()).is_none() {
                    anyhow::bail!("invalid kind '{k}' (expected full | database | media)");
                }
                app.settings
                    .set(&app.db, "backup.auto.kind", k.trim())
                    .await?;
            }
            if let Some(k) = keep {
                if k == 0 {
                    anyhow::bail!("keep must be at least 1");
                }
                app.settings
                    .set(&app.db, "backup.auto.keep", &k.to_string())
                    .await?;
            }
            println!("✦ schedule updated (checked once per minute while the server runs)");
        }
    }
    Ok(())
}
