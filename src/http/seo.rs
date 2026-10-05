//! SEO endpoints: sitemap.xml, robots.txt, rss.xml, atom.xml.
//!
//! Feeds and the sitemap are cached (cache-aside, single-flight) keyed by
//! the effective base URL — the configured `site.base_url` or the request
//! Host header. Content mutations invalidate them immediately via a
//! namespace version bump; the TTL is only a safety net.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::cache::ns;
use crate::error::{AppError, AppResult};
use crate::models::TermKind;
use crate::repositories::{pages, posts as posts_repo, terms};
use crate::state::App;
use crate::utils::{time, xml};

use super::base_url_for;

const FEED_LIMIT: i64 = 20;

fn xml_response(content_type: &'static str, body: String) -> Response {
    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static(content_type),
        )],
        body,
    )
        .into_response()
}

pub async fn rss(State(app): State<App>, headers: HeaderMap) -> AppResult<Response> {
    let base = base_url_for(&app, &headers);
    let body = app
        .cache
        .get_or_load::<String, _>(ns::RSS, &base, async { build_rss(&app, &base).await })
        .await
        .map_err(AppError::Internal)?;
    Ok(xml_response("application/rss+xml; charset=utf-8", body))
}

async fn build_rss(app: &App, base: &str) -> anyhow::Result<String> {
    let site_title = xml::esc(&app.site_title());
    let site_desc = xml::esc(&app.site_description());
    let posts = posts_repo::latest_published(&app.db, FEED_LIMIT).await?;

    let mut items = String::new();
    for p in &posts {
        let url = format!("{}/posts/{}", base, p.slug);
        let ts = p.published_at.unwrap_or(p.created_at);
        let description = if p.summary.is_empty() {
            crate::markdown::truncate_chars(
                &crate::markdown::html_to_text(&crate::markdown::to_html(&p.content_md)),
                280,
            )
        } else {
            p.summary.clone()
        };
        items.push_str(&format!(
            "    <item>\n      <title>{}</title>\n      <link>{}</link>\n      <guid isPermaLink=\"true\">{}</guid>\n      <pubDate>{}</pubDate>\n      <description>{}</description>\n    </item>\n",
            xml::esc(&p.title),
            xml::esc(&url),
            xml::esc(&url),
            time::format(ts, "rfc822"),
            xml::esc(&description),
        ));
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\">\n  <channel>\n    <title>{site_title}</title>\n    <link>{base}</link>\n    <description>{site_desc}</description>\n    <lastBuildDate>{}</lastBuildDate>\n    <generator>Polaris</generator>\n{items}  </channel>\n</rss>\n",
        time::format(time::now(), "rfc822"),
    ))
}

pub async fn atom(State(app): State<App>, headers: HeaderMap) -> AppResult<Response> {
    let base = base_url_for(&app, &headers);
    let body = app
        .cache
        .get_or_load::<String, _>(ns::ATOM, &base, async { build_atom(&app, &base).await })
        .await
        .map_err(AppError::Internal)?;
    Ok(xml_response("application/atom+xml; charset=utf-8", body))
}

async fn build_atom(app: &App, base: &str) -> anyhow::Result<String> {
    let site_title = xml::esc(&app.site_title());
    let posts = posts_repo::latest_published(&app.db, FEED_LIMIT).await?;

    let mut entries = String::new();
    for p in &posts {
        let url = format!("{}/posts/{}", base, p.slug);
        let ts = p.published_at.unwrap_or(p.created_at);
        entries.push_str(&format!(
            "  <entry>\n    <title>{}</title>\n    <link href=\"{}\" rel=\"alternate\" type=\"text/html\"/>\n    <id>{}</id>\n    <updated>{}</updated>\n    <published>{}</published>\n    <author><name>{}</name></author>\n    <summary>{}</summary>\n  </entry>\n",
            xml::esc(&p.title),
            xml::esc(&url),
            xml::esc(&url),
            time::format(p.updated_at, "rfc3339"),
            time::format(ts, "rfc3339"),
            xml::esc(p.author_name.as_deref().unwrap_or("")),
            xml::esc(&p.summary),
        ));
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<feed xmlns=\"http://www.w3.org/2005/Atom\">\n  <title>{site_title}</title>\n  <id>{base}/atom.xml</id>\n  <updated>{}</updated>\n  <generator>Polaris</generator>\n{entries}</feed>\n",
        time::format(time::now(), "rfc3339"),
    ))
}

pub async fn sitemap(State(app): State<App>, headers: HeaderMap) -> AppResult<Response> {
    let base = base_url_for(&app, &headers);
    let body = app
        .cache
        .get_or_load::<String, _>(ns::SITEMAP, &base, async {
            build_sitemap(&app, &base).await
        })
        .await
        .map_err(AppError::Internal)?;
    Ok(xml_response("application/xml; charset=utf-8", body))
}

async fn build_sitemap(app: &App, base: &str) -> anyhow::Result<String> {
    let mut urls = String::new();
    urls.push_str(&format!(
        "  <url>\n    <loc>{}</loc>\n  </url>\n",
        xml::esc(base)
    ));
    for p in pages::list(&app.db, true).await? {
        urls.push_str(&format!(
            "  <url>\n    <loc>{}/{}</loc>\n    <lastmod>{}</lastmod>\n  </url>\n",
            xml::esc(base),
            xml::esc(&p.slug),
            time::format(p.updated_at, "rfc3339"),
        ));
    }
    for p in posts_repo::latest_published(&app.db, 5000).await? {
        urls.push_str(&format!(
            "  <url>\n    <loc>{}/posts/{}</loc>\n    <lastmod>{}</lastmod>\n  </url>\n",
            xml::esc(base),
            xml::esc(&p.slug),
            time::format(p.updated_at, "rfc3339"),
        ));
    }
    for kind in [TermKind::Category, TermKind::Tag] {
        let prefix = match kind {
            TermKind::Category => "category",
            TermKind::Tag => "tag",
        };
        for t in terms::list_with_counts(&app.db, kind).await? {
            urls.push_str(&format!(
                "  <url>\n    <loc>{}/{}/{}</loc>\n  </url>\n",
                xml::esc(base),
                prefix,
                xml::esc(&t.slug)
            ));
        }
    }
    Ok(format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n{urls}</urlset>\n"
    ))
}

pub async fn robots(State(app): State<App>, headers: HeaderMap) -> AppResult<Response> {
    let base = base_url_for(&app, &headers);
    let body = format!(
        "User-agent: *\nAllow: /\nDisallow: /admin\nDisallow: /api\n\nSitemap: {base}/sitemap.xml\n"
    );
    Ok(xml_response("text/plain; charset=utf-8", body))
}
