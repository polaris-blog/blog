use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::any::AnyRow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Author,
    Editor,
    Admin,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::Editor => "editor",
            Self::Admin => "admin",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "admin" => Some(Self::Admin),
            "editor" => Some(Self::Editor),
            "author" => Some(Self::Author),
            _ => None,
        }
    }

    pub fn at_least(self, other: Self) -> bool {
        self >= other
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct User {
    pub id: i64,
    pub username: String,
    pub email: String,
    #[serde(skip_serializing)]
    pub password_hash: String,
    pub role: Role,
    pub display_name: String,
    pub bio: String,
    pub created_at: i64,
    pub updated_at: i64,
}

impl User {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        let role_raw: String = r.try_get("role")?;
        Ok(Self {
            id: r.try_get("id")?,
            username: r.try_get("username")?,
            email: r.try_get("email")?,
            password_hash: r.try_get("password_hash")?,
            role: Role::parse(&role_raw).unwrap_or(Role::Author),
            display_name: r.try_get("display_name")?,
            bio: crate::db::text(r, "bio")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
        })
    }

    pub fn display(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PostStatus {
    #[default]
    Draft,
    Scheduled,
    Published,
}

impl PostStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Scheduled => "scheduled",
            Self::Published => "published",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "draft" => Some(Self::Draft),
            "scheduled" => Some(Self::Scheduled),
            "published" => Some(Self::Published),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TermKind {
    Category,
    Tag,
}

impl TermKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Category => "category",
            Self::Tag => "tag",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "category" => Some(Self::Category),
            "tag" => Some(Self::Tag),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Term {
    pub id: i64,
    pub kind: TermKind,
    pub name: String,
    pub slug: String,
    /// Post count for this term (term listings only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
}

impl Term {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        let kind_raw: String = r.try_get("kind")?;
        Ok(Self {
            id: r.try_get("id")?,
            kind: TermKind::parse(&kind_raw).unwrap_or(TermKind::Tag),
            name: r.try_get("name")?,
            slug: r.try_get("slug")?,
            count: r
                .try_get::<i64, _>("cnt")
                .or_else(|_| r.try_get::<i64, _>("count"))
                .ok(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Post {
    pub id: i64,
    pub title: String,
    pub slug: String,
    pub summary: String,
    /// Skipped in API JSON; the cache layer round-trips it separately.
    #[serde(default, skip_serializing)]
    pub content_md: String,
    pub author_id: i64,
    pub status: PostStatus,
    pub featured_image: Option<String>,
    pub published_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Joined author display name (populated by queries that need it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
    /// Terms attached to this post (populated on demand).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<Term>,
}

impl Post {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        let status_raw: String = r.try_get("status")?;
        Ok(Self {
            id: r.try_get("id")?,
            title: r.try_get("title")?,
            slug: r.try_get("slug")?,
            summary: crate::db::text(r, "summary")?,
            content_md: crate::db::text(r, "content_md")?,
            author_id: r.try_get("author_id")?,
            status: PostStatus::parse(&status_raw).unwrap_or(PostStatus::Draft),
            featured_image: r.try_get("featured_image")?,
            published_at: r.try_get("published_at")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
            author_name: r.try_get::<String, _>("author_name").ok(),
            terms: Vec::new(),
        })
    }

    pub fn category(&self) -> Option<&Term> {
        self.terms.iter().find(|t| t.kind == TermKind::Category)
    }

    pub fn tags(&self) -> Vec<&Term> {
        self.terms
            .iter()
            .filter(|t| t.kind == TermKind::Tag)
            .collect()
    }

    pub fn reading_time(&self) -> i64 {
        1 + (self.content_md.chars().count() as i64) / 1000
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub id: i64,
    pub title: String,
    pub slug: String,
    pub summary: String,
    #[serde(default, skip_serializing)]
    pub content_md: String,
    pub author_id: i64,
    pub status: PostStatus,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author_name: Option<String>,
}

impl Page {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        let status_raw: String = r.try_get("status")?;
        Ok(Self {
            id: r.try_get("id")?,
            title: r.try_get("title")?,
            slug: r.try_get("slug")?,
            summary: crate::db::text(r, "summary")?,
            content_md: crate::db::text(r, "content_md")?,
            author_id: r.try_get("author_id")?,
            status: PostStatus::parse(&status_raw).unwrap_or(PostStatus::Draft),
            sort_order: r.try_get("sort_order")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
            author_name: r.try_get::<String, _>("author_name").ok(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Image,
    Video,
    Audio,
    Document,
    Archive,
    Other,
}

impl MediaKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Document => "document",
            Self::Archive => "archive",
            Self::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            "document" => Some(Self::Document),
            "archive" => Some(Self::Archive),
            "other" => Some(Self::Other),
            _ => None,
        }
    }

    /// Classify a (sniffed) MIME type. Never trusts the client.
    pub fn from_mime(mime: &str) -> Self {
        let mime = mime.split(';').next().unwrap_or("").trim();
        if mime.starts_with("image/") {
            Self::Image
        } else if mime.starts_with("video/") {
            Self::Video
        } else if mime.starts_with("audio/") {
            Self::Audio
        } else if mime == "application/pdf" || mime.starts_with("text/") {
            Self::Document
        } else if matches!(
            mime,
            "application/zip"
                | "application/gzip"
                | "application/x-7z-compressed"
                | "application/x-rar-compressed"
                | "application/x-tar"
        ) {
            Self::Archive
        } else {
            Self::Other
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Media {
    pub id: i64,
    /// Short random identifier used in public URLs (immutable).
    pub uuid: String,
    /// Display filename (editable; independent of the storage key).
    pub filename: String,
    pub original_filename: String,
    /// StorageProvider-relative key, e.g. `2026/08/8c7d2f91.webp`.
    pub storage_key: String,
    pub mime_type: String,
    pub extension: String,
    pub size: i64,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration: Option<i64>,
    /// SHA-256 of the stored bytes (dedup + ETag source).
    pub hash: String,
    pub title: String,
    pub description: String,
    pub alt: String,
    pub caption: String,
    /// Generated thumbnail size names (thumb/small/medium/large).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub thumbnails: Vec<String>,
    pub folder_id: Option<i64>,
    pub uploaded_by: i64,
    pub created_at: i64,
    pub updated_at: i64,
    /// Joined uploader display name (listings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploader_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Number of posts/pages embedding this item (listings/detail).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_count: Option<i64>,
}

impl Media {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        let thumbnails: String = crate::db::text(r, "thumbnails")?;
        Ok(Self {
            id: r.try_get("id")?,
            uuid: r.try_get("uuid")?,
            filename: r.try_get("filename")?,
            original_filename: r.try_get("original_filename")?,
            storage_key: r.try_get("storage_key")?,
            mime_type: r.try_get("mime_type")?,
            extension: r.try_get("extension")?,
            size: r.try_get("size")?,
            width: r.try_get("width")?,
            height: r.try_get("height")?,
            duration: r.try_get("duration")?,
            hash: r.try_get("hash")?,
            title: r.try_get("title")?,
            description: crate::db::text(r, "description")?,
            alt: crate::db::text(r, "alt")?,
            caption: crate::db::text(r, "caption")?,
            thumbnails: thumbnails
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            folder_id: r.try_get("folder_id")?,
            uploaded_by: r.try_get("uploaded_by")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
            uploader_name: r.try_get::<String, _>("uploader_name").ok(),
            folder_name: r.try_get::<String, _>("folder_name").ok(),
            tags: Vec::new(),
            ref_count: r.try_get::<i64, _>("ref_count").ok(),
        })
    }

    pub fn kind(&self) -> MediaKind {
        MediaKind::from_mime(&self.mime_type)
    }

    /// Public URL of the original (relative path; CDN prefix applied by the
    /// URL builder in the media service).
    pub fn url_path(&self) -> String {
        format!("/media/{}.{}", self.uuid, self.extension)
    }

    /// Public URL of a generated thumbnail size.
    pub fn thumb_url_path(&self, size: &str) -> String {
        format!("/media/{}.{}.{}", self.uuid, size, self.extension)
    }

    /// Thumbnail storage key (`2026/08/uuid.small.webp` style).
    pub fn thumb_storage_key(&self, size: &str) -> String {
        match self.storage_key.rsplit_once('.') {
            Some((base, ext)) => format!("{base}.{size}.{ext}"),
            None => format!("{}.{}", self.storage_key, size),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaFolder {
    pub id: i64,
    pub name: String,
    pub slug: String,
    pub created_at: i64,
    /// Media count (populated in listings).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<i64>,
}

impl MediaFolder {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        Ok(Self {
            id: r.try_get("id")?,
            name: r.try_get("name")?,
            slug: r.try_get("slug")?,
            created_at: r.try_get("created_at")?,
            count: r
                .try_get::<i64, _>("cnt")
                .or_else(|_| r.try_get::<i64, _>("count"))
                .ok(),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentStatus {
    Pending,
    Approved,
    Spam,
}

impl CommentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Spam => "spam",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "spam" => Some(Self::Spam),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Comment {
    pub id: i64,
    pub post_id: i64,
    pub parent_id: Option<i64>,
    pub author_name: String,
    pub author_email: String,
    pub author_url: String,
    pub content: String,
    pub status: CommentStatus,
    pub created_at: i64,
    /// Post title (populated in admin listings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_title: Option<String>,
    /// Post slug (populated in admin listings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub post_slug: Option<String>,
}

impl Comment {
    pub fn from_row(r: &AnyRow) -> sqlx::Result<Self> {
        let status_raw: String = r.try_get("status")?;
        Ok(Self {
            id: r.try_get("id")?,
            post_id: r.try_get("post_id")?,
            parent_id: r.try_get("parent_id")?,
            author_name: r.try_get("author_name")?,
            author_email: r.try_get("author_email")?,
            author_url: r.try_get("author_url")?,
            content: crate::db::text(r, "content")?,
            status: CommentStatus::parse(&status_raw).unwrap_or(CommentStatus::Pending),
            created_at: r.try_get("created_at")?,
            post_title: r.try_get::<String, _>("post_title").ok(),
            post_slug: r.try_get::<String, _>("post_slug").ok(),
        })
    }
}
