//! Extension service: orchestration between the installer (files + registry)
//! and the running theme/plugin managers (hot reload).

use std::path::Path;

use serde_json::json;

use crate::error::{AppError, AppResult};
use crate::extension::installer::{ExtensionInstaller, InstallAction, InstallOptions};
use crate::extension::{ExtensionKind, ExtensionManifest};
use crate::state::App;

use crate::config_store::plugin_ns;

pub fn installer(app: &App) -> ExtensionInstaller {
    ExtensionInstaller::new(app.db.clone(), &app.config)
}

/// Install (or update/downgrade) a package ZIP. After a successful install
/// the affected runtime is reloaded: enabled plugins pick up new code, and an
/// updated *active* theme re-renders from its new templates. Newly installed
/// extensions are never auto-activated/enabled.
pub async fn install(
    app: &App,
    zip_path: &Path,
    actor: &str,
    force: bool,
    allow_downgrade: bool,
) -> AppResult<crate::extension::InstallOutcome> {
    let mut opts = InstallOptions::new(actor);
    opts.force = force;
    opts.allow_downgrade = allow_downgrade;
    let installer = installer(app);
    let out = installer.install(zip_path, &opts).await?;

    match out.kind {
        ExtensionKind::Plugin => {
            // Load the new package's config schema and reload it if enabled.
            if let Err(e) = app
                .configs
                .load_plugin(Path::new(&app.config.plugin.dir), &out.manifest.id)
                .await
            {
                tracing::warn!(plugin = out.manifest.id.as_str(), error = %e, "plugin config schema failed to load");
            }
            if app.settings.plugins_enabled().contains(&out.manifest.id) {
                let names = app.settings.plugins_enabled();
                app.set_plugins_enabled(&names).await?;
            }
        }
        ExtensionKind::Theme => {
            let active = app.theme.current_name();
            if active == out.manifest.id {
                app.theme.activate(&active)?;
                if let Err(e) = app
                    .configs
                    .load_theme(Path::new(&app.config.theme.dir), &active)
                    .await
                {
                    tracing::warn!(theme = active.as_str(), error = %e, "theme config schema failed to load");
                }
                app.invalidate_content().await;
            }
        }
    }
    Ok(out)
}

/// Uninstall an extension. The active theme cannot be removed; an enabled
/// plugin is disabled first. Plugin data survives unless `remove_data`.
pub async fn uninstall(
    app: &App,
    kind: ExtensionKind,
    id: &str,
    remove_data: bool,
    actor: &str,
) -> AppResult<crate::extension::UninstallOutcome> {
    if kind == ExtensionKind::Theme && app.theme.current_name() == id {
        return Err(AppError::BadRequest(
            "cannot uninstall the active theme — activate another theme first".into(),
        ));
    }
    if kind == ExtensionKind::Plugin && app.settings.plugins_enabled().iter().any(|n| n == id) {
        let mut names = app.settings.plugins_enabled();
        names.retain(|n| n != id);
        app.set_plugins_enabled(&names).await?;
        app.configs.unload_namespace(&plugin_ns(id));
    }
    installer(app).uninstall(kind, id, remove_data, actor).await
}

fn manifest_json(m: &ExtensionManifest) -> serde_json::Value {
    json!({
        "id": m.id,
        "name": m.name,
        "version": m.version.to_string(),
        "author": m.author,
        "description": m.description,
        "license": m.license,
        "homepage": m.homepage,
        "repository": m.repository,
        "permissions": m.permissions,
        "dependencies": m.dependencies.iter()
            .map(|(id, req)| json!({"id": id, "requirement": req}))
            .collect::<Vec<_>>(),
        "requires_polaris": match (&m.minimum_polaris_version, &m.maximum_polaris_version) {
            (Some(min), Some(max)) => format!(">= {min}, <= {max}"),
            (Some(min), None) => format!(">= {min}"),
            (None, Some(max)) => format!("<= {max}"),
            (None, None) => String::new(),
        },
        "entry": m.entry,
    })
}

/// Status of every extension of one kind: disk scan joined with the registry
/// and the runtime (active theme / enabled plugins).
pub async fn status(app: &App, kind: ExtensionKind) -> AppResult<Vec<serde_json::Value>> {
    let installer = installer(app);
    let records = crate::repositories::extensions::list(&app.db).await?;
    let active_theme = app.theme.current_name();
    let enabled: Vec<String> = app.settings.plugins_enabled();

    let mut out = Vec::new();
    for entry in installer.scan(kind) {
        let record = records
            .iter()
            .find(|r| r.kind == kind.as_str() && r.ext_id == entry.id);
        let (manifest, broken) = match (&entry.manifest, &entry.error) {
            (Some(m), _) => (manifest_json(m), false),
            (None, Some(_err)) => (
                json!({"id": entry.id, "name": entry.id, "version": "", "permissions": []}),
                true,
            ),
            (None, None) => continue,
        };
        let installed_at = record.map(|r| r.installed_at).unwrap_or_default();
        let updated_at = record.map(|r| r.updated_at).unwrap_or_default();
        let status = if broken {
            "broken"
        } else {
            match kind {
                ExtensionKind::Theme if entry.id == active_theme => "active",
                ExtensionKind::Plugin if enabled.contains(&entry.id) => "enabled",
                _ => "disabled",
            }
        };
        let has_settings = crate::config_store::ConfigManager::has_schema(
            match kind {
                ExtensionKind::Theme => Path::new(&app.config.theme.dir),
                ExtensionKind::Plugin => Path::new(&app.config.plugin.dir),
            },
            &entry.id,
        );
        out.push(json!({
            "dir": entry.id,
            "manifest": manifest,
            "status": status,
            "registry_version": record.map(|r| r.version.clone()).unwrap_or_default(),
            "installed_at": installed_at,
            "updated_at": updated_at,
            "error": entry.error.clone().unwrap_or_default(),
            "has_settings": has_settings,
            "is_active_theme": kind == ExtensionKind::Theme && entry.id == active_theme,
            "is_enabled_plugin": kind == ExtensionKind::Plugin && enabled.contains(&entry.id),
        }));
    }
    Ok(out)
}

pub fn outcome_json(out: &crate::extension::InstallOutcome) -> serde_json::Value {
    json!({
        "kind": out.kind.as_str(),
        "id": out.manifest.id,
        "name": out.manifest.name,
        "version": out.manifest.version.to_string(),
        "action": out.action.as_str(),
        "previous_version": match &out.action {
            InstallAction::Installed => None,
            InstallAction::Updated { from } | InstallAction::Downgraded { from } => Some(from.to_string()),
        },
        "sha256": out.package_hash,
        "permissions": out.manifest.permissions,
        "warnings": out.warnings,
        "backup": out.backup.as_ref().map(|b| b.display().to_string()),
    })
}

/// Verify every installed extension (manifest, registry, plugin entry file).
pub async fn verify(app: &App) -> AppResult<Vec<crate::extension::installer::VerifyEntry>> {
    installer(app).verify().await
}

/// Recent extension audit log entries.
pub async fn logs(
    app: &App,
    kind: Option<ExtensionKind>,
    limit: i64,
) -> AppResult<Vec<crate::repositories::extensions::ExtensionLogEntry>> {
    installer(app).logs(kind, limit).await
}

/// Human-readable summary for flash messages / CLI output.
pub fn outcome_summary(out: &crate::extension::InstallOutcome) -> String {
    use crate::i18n::tr;
    let what = match out.kind {
        ExtensionKind::Theme => tr("ext.summary.theme", &[]),
        ExtensionKind::Plugin => tr("ext.summary.plugin", &[]),
    };
    let verb = match &out.action {
        InstallAction::Installed => tr("ext.summary.installed", &[]),
        InstallAction::Updated { from } => {
            let from = from.to_string();
            tr("ext.summary.updated", &[("from", &from)])
        }
        InstallAction::Downgraded { from } => {
            let from = from.to_string();
            tr("ext.summary.downgraded", &[("from", &from)])
        }
    };
    let perms = if out.manifest.permissions.is_empty() {
        String::new()
    } else {
        let joined = out.manifest.permissions.join(", ");
        tr("ext.summary.perms", &[("perms", &joined)])
    };
    format!(
        "{what} '{}' v{} {}{perms}",
        out.manifest.name, out.manifest.version, verb
    )
}
