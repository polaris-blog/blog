//! Media repository — metadata only; bytes live in the StorageProvider.

use sqlx::Row;

use crate::db::{Bind, Db};
use crate::error::AppResult;
use crate::models::{Media, MediaFolder, MediaKind};
use crate::utils::time;

const COLS: &str = "m.id, m.uuid, m.filename, m.original_filename, m.storage_key, m.mime_type, \
                    m.extension, m.size, m.width, m.height, m.duration, m.hash, m.title, \
                    m.description, m.alt, m.caption, m.thumbnails, m.folder_id, m.uploaded_by, \
                    m.created_at, m.updated_at, \
                    COALESCE(NULLIF(u.display_name, ''), u.username) AS uploader_name, \
                    f.name AS folder_name, \
                    (SELECT COUNT(*) FROM media_references r WHERE r.media_id = m.id) AS ref_count";

const FROM: &str = "FROM media m JOIN users u ON u.id = m.uploaded_by \
                    LEFT JOIN media_folders f ON f.id = m.folder_id";

pub struct NewMedia {
    pub uuid: String,
    pub filename: String,
    pub original_filename: String,
    pub storage_key: String,
    pub mime_type: String,
    pub extension: String,
    pub size: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    pub hash: String,
    pub thumbnails: Vec<String>,
    pub folder_id: Option<i64>,
    pub uploaded_by: i64,
}

pub async fn insert(db: &Db, m: &NewMedia) -> AppResult<i64> {
    let now = time::now();
    let thumbnails = m.thumbnails.join(",");
    db.insert(
        "INSERT INTO media (uuid, filename, original_filename, storage_key, mime_type, \
         extension, size, width, height, duration, hash, title, description, alt, caption, \
         thumbnails, folder_id, uploaded_by, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '', '', '', '', ?, ?, ?, ?, ?)",
        &[
            Bind::S(m.uuid.clone()),
            Bind::S(m.filename.clone()),
            Bind::S(m.original_filename.clone()),
            Bind::S(m.storage_key.clone()),
            Bind::S(m.mime_type.clone()),
            Bind::S(m.extension.clone()),
            Bind::I(m.size),
            Bind::OptI(m.width),
            Bind::OptI(m.height),
            Bind::OptI(m.duration),
            Bind::S(m.hash.clone()),
            Bind::S(thumbnails),
            Bind::OptI(m.folder_id),
            Bind::I(m.uploaded_by),
            Bind::I(now),
            Bind::I(now),
        ],
    )
    .await
}

/// Sort orders for media listings (SQL is built from these variants only —
/// never from user input).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MediaSort {
    #[default]
    Newest,
    Oldest,
    Name,
    Size,
}

impl MediaSort {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "newest" | "created_at" => Some(Self::Newest),
            "oldest" | "oldest_first" => Some(Self::Oldest),
            "name" | "filename" => Some(Self::Name),
            "size" | "largest" => Some(Self::Size),
            _ => None,
        }
    }

    fn order_sql(self) -> &'static str {
        match self {
            Self::Newest => "m.created_at DESC, m.id DESC",
            Self::Oldest => "m.created_at ASC, m.id ASC",
            Self::Name => "LOWER(m.filename) ASC, m.id ASC",
            Self::Size => "m.size DESC, m.id DESC",
        }
    }
}

/// Virtual-folder scoping for listings: all items, only unfiled ones, or a
/// single folder.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum FolderFilter {
    #[default]
    All,
    Unfiled,
    Id(i64),
}

#[derive(Default)]
pub struct MediaFilter {
    pub kind: Option<MediaKind>,
    pub folder: FolderFilter,
    /// Filter by tag slug/name.
    pub tag: Option<String>,
    /// Free-text search (filename/title/description/alt/caption/tags).
    pub search: Option<String>,
    /// Author scoping: only media uploaded by this user.
    pub uploaded_by: Option<i64>,
    pub sort: MediaSort,
    pub page: i64,
    pub per_page: i64,
}

/// LIKE-escaped search term: `%`/`_` lose their special meaning.
fn like_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.chars() {
        if c == '%' || c == '_' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

pub async fn list(db: &Db, f: &MediaFilter) -> AppResult<(Vec<Media>, i64)> {
    let mut clauses: Vec<String> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();

    if let Some(kind) = f.kind {
        // Static literals only (no user input) — safe to inline.
        let clause = match kind {
            MediaKind::Image => "m.mime_type LIKE 'image/%'",
            MediaKind::Video => "m.mime_type LIKE 'video/%'",
            MediaKind::Audio => "m.mime_type LIKE 'audio/%'",
            MediaKind::Document => "(m.mime_type = 'application/pdf' OR m.mime_type LIKE 'text/%')",
            MediaKind::Archive => {
                "(m.mime_type IN ('application/zip', 'application/gzip', \
                  'application/x-7z-compressed', 'application/x-rar-compressed', \
                  'application/x-tar'))"
            }
            MediaKind::Other => {
                "(m.mime_type NOT LIKE 'image/%' AND m.mime_type NOT LIKE 'video/%' AND \
                  m.mime_type NOT LIKE 'audio/%' AND m.mime_type <> 'application/pdf' AND \
                  m.mime_type NOT LIKE 'text/%' AND m.mime_type NOT IN ('application/zip', \
                  'application/gzip', 'application/x-7z-compressed', \
                  'application/x-rar-compressed', 'application/x-tar'))"
            }
        };
        clauses.push(clause.into());
    }
    match f.folder {
        FolderFilter::All => {}
        FolderFilter::Unfiled => clauses.push("m.folder_id IS NULL".into()),
        FolderFilter::Id(id) => {
            clauses.push("m.folder_id = ?".into());
            binds.push(Bind::I(id));
        }
    }
    if let Some(tag) = f.tag.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        clauses.push(
            "EXISTS (SELECT 1 FROM media_tags mt WHERE mt.media_id = m.id AND mt.tag = ?)".into(),
        );
        binds.push(Bind::S(tag.to_string()));
    }
    if let Some(uploader) = f.uploaded_by {
        clauses.push("m.uploaded_by = ?".into());
        binds.push(Bind::I(uploader));
    }
    if let Some(search) = f.search.as_deref().map(str::trim).filter(|s| !s.is_empty())
        && search.chars().count() >= 2
    {
        let pat = format!("%{}%", like_escape(search));
        clauses.push(
            "(m.filename LIKE ? OR m.title LIKE ? OR m.description LIKE ? OR m.alt LIKE ? \
                  OR m.caption LIKE ? OR m.mime_type LIKE ? OR EXISTS (SELECT 1 FROM media_tags mt \
                  WHERE mt.media_id = m.id AND mt.tag LIKE ?))"
                .into(),
        );
        for _ in 0..7 {
            binds.push(Bind::S(pat.clone()));
        }
    }

    let where_sql = if clauses.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", clauses.join(" AND "))
    };

    let total = db
        .fetch_one(
            &format!("SELECT COUNT(*) AS total {FROM} {where_sql}"),
            &binds,
        )
        .await?
        .try_get::<i64, _>("total")
        .map_err(crate::error::AppError::Db)?;

    let page = f.page.max(1);
    let per = f.per_page.clamp(1, 200);
    let offset = (page - 1).saturating_mul(per);
    let mut qbinds = binds;
    qbinds.push(Bind::I(per));
    qbinds.push(Bind::I(offset));
    let rows = db
        .fetch_all(
            &format!(
                "SELECT {COLS} {FROM} {where_sql} ORDER BY {} LIMIT ? OFFSET ?",
                f.sort.order_sql()
            ),
            &qbinds,
        )
        .await?;
    let mut media = rows
        .iter()
        .map(Media::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)?;
    attach_tags(db, &mut media).await?;
    Ok((media, total))
}

pub async fn find_by_id(db: &Db, id: i64) -> AppResult<Option<Media>> {
    let row = db
        .fetch_optional(
            &format!("SELECT {COLS} {FROM} WHERE m.id = ?"),
            &[Bind::I(id)],
        )
        .await?;
    let mut media = row
        .map(|r| Media::from_row(&r).map_err(crate::error::AppError::Db))
        .transpose()?;
    if let Some(m) = &mut media {
        attach_tags(db, std::slice::from_mut(m)).await?;
    }
    Ok(media)
}

/// Resolve a bounded search page in two queries, including tags.
pub async fn find_by_ids(db: &Db, ids: &[i64]) -> AppResult<Vec<Media>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    if ids.len() > 100 {
        return Err(crate::error::AppError::BadRequest(
            "too many media ids".into(),
        ));
    }
    let placeholders = vec!["?"; ids.len()].join(",");
    let binds: Vec<_> = ids.iter().copied().map(Bind::I).collect();
    let rows = db
        .fetch_all(
            &format!("SELECT {COLS} {FROM} WHERE m.id IN ({placeholders})"),
            &binds,
        )
        .await?;
    let mut media = rows
        .iter()
        .map(Media::from_row)
        .collect::<sqlx::Result<Vec<_>>>()?;
    attach_tags(db, &mut media).await?;
    Ok(media)
}

pub async fn find_by_uuid(db: &Db, uuid: &str) -> AppResult<Option<Media>> {
    let row = db
        .fetch_optional(
            &format!("SELECT {COLS} {FROM} WHERE m.uuid = ?"),
            &[Bind::S(uuid.to_string())],
        )
        .await?;
    row.map(|r| Media::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn find_by_hash(db: &Db, hash: &str) -> AppResult<Option<Media>> {
    let row = db
        .fetch_optional(
            &format!("SELECT {COLS} {FROM} WHERE m.hash = ?"),
            &[Bind::S(hash.to_string())],
        )
        .await?;
    row.map(|r| Media::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn find_by_hash_for_user(db: &Db, hash: &str, user_id: i64) -> AppResult<Option<Media>> {
    let row = db
        .fetch_optional(
            &format!(
                "SELECT {COLS} {FROM} WHERE m.hash = ? AND m.uploaded_by = ? ORDER BY m.id LIMIT 1"
            ),
            &[Bind::S(hash.to_string()), Bind::I(user_id)],
        )
        .await?;
    row.map(|r| Media::from_row(&r).map_err(Into::into))
        .transpose()
}

/// Iterate all media in id order (CLI maintenance: verify/orphan/cleanup).
pub async fn scan(db: &Db, last_id: i64, limit: i64) -> AppResult<Vec<Media>> {
    let rows = db
        .fetch_all(
            &format!("SELECT {COLS} {FROM} WHERE m.id > ? ORDER BY m.id LIMIT ?"),
            &[Bind::I(last_id), Bind::I(limit)],
        )
        .await?;
    let mut media = rows
        .iter()
        .map(Media::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(crate::error::AppError::Db)?;
    attach_tags(db, &mut media).await?;
    Ok(media)
}

pub struct MediaUpdate {
    pub filename: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub alt: Option<String>,
    pub caption: Option<String>,
    pub folder_id: Option<Option<i64>>,
    pub tags: Option<Vec<String>>,
}

pub async fn update(db: &Db, id: i64, u: &MediaUpdate) -> AppResult<()> {
    let mut sets: Vec<&str> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(filename) = &u.filename {
        sets.push("filename = ?");
        binds.push(Bind::S(filename.clone()));
    }
    if let Some(title) = &u.title {
        sets.push("title = ?");
        binds.push(Bind::S(title.clone()));
    }
    if let Some(description) = &u.description {
        sets.push("description = ?");
        binds.push(Bind::S(description.clone()));
    }
    if let Some(alt) = &u.alt {
        sets.push("alt = ?");
        binds.push(Bind::S(alt.clone()));
    }
    if let Some(caption) = &u.caption {
        sets.push("caption = ?");
        binds.push(Bind::S(caption.clone()));
    }
    if let Some(folder) = &u.folder_id {
        sets.push("folder_id = ?");
        binds.push(Bind::OptI(*folder));
    }
    if sets.is_empty() && u.tags.is_none() {
        return Ok(());
    }
    if !sets.is_empty() {
        sets.push("updated_at = ?");
        binds.push(Bind::I(time::now()));
        binds.push(Bind::I(id));
        db.execute(
            &format!("UPDATE media SET {} WHERE id = ?", sets.join(", ")),
            &binds,
        )
        .await?;
    }
    if let Some(tags) = &u.tags {
        set_tags(db, id, tags).await?;
    }
    Ok(())
}

pub async fn delete(db: &Db, id: i64) -> AppResult<bool> {
    let n = db
        .execute("DELETE FROM media WHERE id = ?", &[Bind::I(id)])
        .await?;
    Ok(n > 0)
}

/// All storage keys owned by a media row (original + thumbnails).
pub fn all_storage_keys(m: &Media) -> Vec<String> {
    let mut keys = vec![m.storage_key.clone()];
    for size in &m.thumbnails {
        keys.push(m.thumb_storage_key(size));
    }
    keys
}

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

pub async fn attach_tags(db: &Db, media: &mut [Media]) -> AppResult<()> {
    if media.is_empty() {
        return Ok(());
    }
    let ids: Vec<String> = media.iter().map(|m| m.id.to_string()).collect();
    let rows = db
        .fetch_all(
            &format!(
                "SELECT media_id, tag FROM media_tags WHERE media_id IN ({}) ORDER BY tag",
                ids.join(",")
            ),
            &[],
        )
        .await?;
    for r in rows {
        let id: i64 = r.try_get("media_id").map_err(crate::error::AppError::Db)?;
        let tag: String = r.try_get("tag").map_err(crate::error::AppError::Db)?;
        if let Some(m) = media.iter_mut().find(|m| m.id == id) {
            m.tags.push(tag);
        }
    }
    Ok(())
}

/// Replace the tag set of a media item.
pub async fn set_tags(db: &Db, id: i64, tags: &[String]) -> AppResult<()> {
    db.execute("DELETE FROM media_tags WHERE media_id = ?", &[Bind::I(id)])
        .await?;
    for tag in tags {
        let tag = tag.trim().to_string();
        if tag.is_empty() || tag.len() > 100 {
            continue;
        }
        db.execute(
            "INSERT INTO media_tags (media_id, tag) VALUES (?, ?)",
            &[Bind::I(id), Bind::S(tag)],
        )
        .await?;
    }
    Ok(())
}

/// Distinct tags with usage counts (filter chips).
pub async fn tag_counts(db: &Db) -> AppResult<Vec<(String, i64)>> {
    let rows = db
        .fetch_all(
            "SELECT tag, COUNT(*) AS total FROM media_tags GROUP BY tag ORDER BY COUNT(*) DESC, tag",
            &[],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let tag: String = r.try_get("tag").map_err(crate::error::AppError::Db)?;
        let total: i64 = r.try_get("total").map_err(crate::error::AppError::Db)?;
        out.push((tag, total));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Folders (virtual — purely metadata, never a filesystem path)
// ---------------------------------------------------------------------------

pub async fn folders(db: &Db) -> AppResult<Vec<MediaFolder>> {
    let rows = db
        .fetch_all(
            "SELECT f.id, f.name, f.slug, f.created_at, \
             (SELECT COUNT(*) FROM media m WHERE m.folder_id = f.id) AS count \
             FROM media_folders f ORDER BY f.name",
            &[],
        )
        .await?;
    rows.iter()
        .map(MediaFolder::from_row)
        .collect::<sqlx::Result<Vec<_>>>()
        .map_err(Into::into)
}

pub async fn folder_find(db: &Db, id: i64) -> AppResult<Option<MediaFolder>> {
    let row = db
        .fetch_optional(
            "SELECT f.id, f.name, f.slug, f.created_at, \
             (SELECT COUNT(*) FROM media m WHERE m.folder_id = f.id) AS count \
             FROM media_folders f WHERE f.id = ?",
            &[Bind::I(id)],
        )
        .await?;
    row.map(|r| MediaFolder::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn folder_find_by_slug(db: &Db, slug: &str) -> AppResult<Option<MediaFolder>> {
    let row = db
        .fetch_optional(
            "SELECT f.id, f.name, f.slug, f.created_at, \
             (SELECT COUNT(*) FROM media m WHERE m.folder_id = f.id) AS count \
             FROM media_folders f WHERE f.slug = ?",
            &[Bind::S(slug.to_string())],
        )
        .await?;
    row.map(|r| MediaFolder::from_row(&r).map_err(Into::into))
        .transpose()
}

pub async fn folder_create(db: &Db, name: &str, slug: &str) -> AppResult<i64> {
    db.insert(
        "INSERT INTO media_folders (name, slug, created_at) VALUES (?, ?, ?)",
        &[
            Bind::S(name.to_string()),
            Bind::S(slug.to_string()),
            Bind::I(time::now()),
        ],
    )
    .await
}

pub async fn folder_delete(db: &Db, id: i64) -> AppResult<bool> {
    // Media rows fall back to "unfiled" (ON DELETE SET NULL).
    let n = db
        .execute("DELETE FROM media_folders WHERE id = ?", &[Bind::I(id)])
        .await?;
    Ok(n > 0)
}

// ---------------------------------------------------------------------------
// References (which posts/pages embed a media item)
// ---------------------------------------------------------------------------

/// Replace the reference set recorded for a post or page. Called on save;
/// the content markdown is scanned for `/media/{uuid}` URLs by the service.
pub async fn set_references(
    db: &Db,
    ref_type: &str,
    ref_id: i64,
    media_ids: &[i64],
) -> AppResult<()> {
    db.execute(
        "DELETE FROM media_references WHERE ref_type = ? AND ref_id = ?",
        &[Bind::S(ref_type.to_string()), Bind::I(ref_id)],
    )
    .await?;
    let now = time::now();
    for id in media_ids {
        db.execute(
            "INSERT INTO media_references (media_id, ref_type, ref_id, created_at) VALUES (?, ?, ?, ?)",
            &[Bind::I(*id), Bind::S(ref_type.to_string()), Bind::I(ref_id), Bind::I(now)],
        )
        .await?;
    }
    Ok(())
}

pub async fn references_of(db: &Db, media_id: i64) -> AppResult<Vec<(String, i64)>> {
    let rows = db
        .fetch_all(
            "SELECT ref_type, ref_id FROM media_references WHERE media_id = ?",
            &[Bind::I(media_id)],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let t: String = r.try_get("ref_type").map_err(crate::error::AppError::Db)?;
        let id: i64 = r.try_get("ref_id").map_err(crate::error::AppError::Db)?;
        out.push((t, id));
    }
    Ok(out)
}

/// A reference with the embedding content's title/slug (detail pages).
#[derive(Clone, Debug, serde::Serialize)]
pub struct MediaReference {
    pub ref_type: String,
    pub ref_id: i64,
    pub title: String,
    pub slug: String,
}

pub async fn references_detailed(db: &Db, media_id: i64) -> AppResult<Vec<MediaReference>> {
    let rows = db
        .fetch_all(
            "SELECT r.ref_type, r.ref_id, COALESCE(p.title, g.title, '') AS title, \
             COALESCE(p.slug, g.slug, '') AS slug \
             FROM media_references r \
             LEFT JOIN posts p ON r.ref_type = 'post' AND p.id = r.ref_id \
             LEFT JOIN pages g ON r.ref_type = 'page' AND g.id = r.ref_id \
             WHERE r.media_id = ? ORDER BY r.ref_type, r.ref_id",
            &[Bind::I(media_id)],
        )
        .await?;
    let mut out = Vec::new();
    for r in rows {
        out.push(MediaReference {
            ref_type: r.try_get("ref_type").map_err(crate::error::AppError::Db)?,
            ref_id: r.try_get("ref_id").map_err(crate::error::AppError::Db)?,
            title: r.try_get("title").map_err(crate::error::AppError::Db)?,
            slug: r.try_get("slug").map_err(crate::error::AppError::Db)?,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------------

pub async fn counts(db: &Db) -> AppResult<(i64, i64)> {
    let row = db
        .fetch_one(
            "SELECT COUNT(*) AS total, COALESCE(SUM(size), 0) AS bytes FROM media",
            &[],
        )
        .await?;
    let total: i64 = row.try_get("total").map_err(crate::error::AppError::Db)?;
    let bytes: i64 = row.try_get("bytes").map_err(crate::error::AppError::Db)?;
    Ok((total, bytes))
}
