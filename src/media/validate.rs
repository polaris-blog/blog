//! Upload validation — the defense-in-depth layer.
//!
//! Client input (filename, declared MIME type) is never trusted. Every
//! upload is verified by content:
//!
//! ```text
//! filename ──┐
//! extension ─┤
//! MIME type ─┼──► sniff magic bytes ──► whitelist match ──► accept
//! size ──────┤         │                                  └── images: decode
//! content ───┘         └── mismatch / unknown ──► reject
//! ```
//!
//! SVG is special: it is XML that can carry scripts, event handlers and
//! external references, so it runs through an allow-list sanitizer before
//! being stored (or is rejected outright when sanitization is disabled).

use std::borrow::Cow;

use crate::config::{MediaConfig, MediaSvgConfig};
use crate::models::MediaKind;

/// A sniffed, whitelisted file type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileType {
    /// `Borrowed` for the built-in whitelist; `Owned` for admin-configured
    /// extra types (never leaked — one upload must not grow the heap).
    pub extension: Cow<'static, str>,
    pub mime: &'static str,
    pub kind: MediaKind,
}

impl FileType {
    const fn new(extension: &'static str, mime: &'static str, kind: MediaKind) -> Self {
        Self {
            extension: Cow::Borrowed(extension),
            mime,
            kind,
        }
    }
}

/// Built-in whitelist: extension → verified type. Every entry has a magic
/// byte signature (or structural check) in [`sniff`]. Extensions not listed
/// here are rejected unless an admin added them via `extra_types`.
pub static BUILTIN_TYPES: &[FileType] = &[
    // images
    FileType::new("jpg", "image/jpeg", MediaKind::Image),
    FileType::new("jpeg", "image/jpeg", MediaKind::Image),
    FileType::new("png", "image/png", MediaKind::Image),
    FileType::new("gif", "image/gif", MediaKind::Image),
    FileType::new("webp", "image/webp", MediaKind::Image),
    FileType::new("avif", "image/avif", MediaKind::Image),
    FileType::new("svg", "image/svg+xml", MediaKind::Image),
    // video
    FileType::new("mp4", "video/mp4", MediaKind::Video),
    FileType::new("m4v", "video/mp4", MediaKind::Video),
    FileType::new("webm", "video/webm", MediaKind::Video),
    FileType::new("mov", "video/quicktime", MediaKind::Video),
    // audio
    FileType::new("mp3", "audio/mpeg", MediaKind::Audio),
    FileType::new("m4a", "audio/mp4", MediaKind::Audio),
    FileType::new("aac", "audio/aac", MediaKind::Audio),
    FileType::new("flac", "audio/flac", MediaKind::Audio),
    FileType::new("wav", "audio/wav", MediaKind::Audio),
    FileType::new("ogg", "audio/ogg", MediaKind::Audio),
    FileType::new("oga", "audio/ogg", MediaKind::Audio),
    // documents
    FileType::new("pdf", "application/pdf", MediaKind::Document),
    FileType::new("txt", "text/plain", MediaKind::Document),
    FileType::new("csv", "text/csv", MediaKind::Document),
    // archives
    FileType::new("zip", "application/zip", MediaKind::Archive),
    FileType::new("gz", "application/gzip", MediaKind::Archive),
    FileType::new("tgz", "application/gzip", MediaKind::Archive),
    FileType::new("7z", "application/x-7z-compressed", MediaKind::Archive),
    FileType::new("tar", "application/x-tar", MediaKind::Archive),
];

/// Outcome of sniffing the upload's leading bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sniffed {
    /// Recognized magic bytes.
    Known(&'static FileType),
    /// Text-looking content (txt/csv). Only accepted for text extensions.
    Text,
    /// Nothing known.
    Unknown,
}

/// Identify a file by its leading bytes (magic numbers / structure).
pub fn sniff(head: &[u8]) -> Sniffed {
    // Look the whitelisted type up by extension — index-based references
    // into BUILTIN_TYPES proved too fragile when the list changes.
    let by = |ext: &str| match BUILTIN_TYPES.iter().find(|t| t.extension == ext) {
        Some(t) => Sniffed::Known(t),
        None => Sniffed::Unknown,
    };
    if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return by("jpg");
    }
    if head.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        return by("png");
    }
    if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        return by("gif");
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        return by("webp");
    }
    if head.len() >= 12 && &head[4..8] == b"ftyp" {
        return match &head[8..12] {
            b"avif" | b"avis" | b"avio" => by("avif"),
            b"qt  " => by("mov"),
            b"M4A " => by("m4a"),
            b"M4V " | b"isom" | b"iso2" | b"mp41" | b"mp42" | b"dash" | b"avc1" | b"F4V " => {
                by("mp4")
            }
            _ => Sniffed::Unknown,
        };
    }
    if head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]) {
        return by("webm"); // EBML
    }
    if head.starts_with(b"ID3") {
        return by("mp3"); // ID3-tagged
    }
    // Raw MP3 frames: 0xFF Ex/Fx (version/layer bits set).
    if head.len() >= 2 && head[0] == 0xFF && head[1] & 0xE0 == 0xE0 && head[1] & 0x06 != 0 {
        return by("mp3");
    }
    if head.starts_with(b"fLaC") {
        return by("flac");
    }
    if head.starts_with(b"OggS") {
        return by("ogg"); // audio assumed; video/ogg is rare
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return by("wav");
    }
    if head.starts_with(b"%PDF-") {
        return by("pdf");
    }
    if head.starts_with(b"PK\x03\x04")
        || head.starts_with(b"PK\x05\x06")
        || head.starts_with(b"PK\x07\x08")
    {
        return by("zip");
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return by("gz");
    }
    if head.starts_with(b"7z\xBC\xAF\x27\x1C") {
        return by("7z");
    }
    // tar: "ustar" magic at offset 257 (real tar files are >= 1024 bytes,
    // but only 262 head bytes are needed to see the magic).
    if head.len() >= 262 && &head[257..262] == b"ustar" {
        return by("tar");
    }
    if looks_like_text(head) {
        return Sniffed::Text;
    }
    Sniffed::Unknown
}

/// Heuristic text check: no NUL, no control bytes besides tab/LF/CR, mostly
/// printable. Sufficient to distinguish text uploads from binaries.
fn looks_like_text(head: &[u8]) -> bool {
    if head.is_empty() {
        return false;
    }
    head.iter()
        .all(|&b| b == 0x09 || b == 0x0A || b == 0x0D || (0x20..0x7F).contains(&b) || b >= 0x80)
}

/// Cheap pre-check for SVG uploads: an `<svg` opening tag appears in the
/// head bytes (after an optional XML declaration / DOCTYPE). The full
/// allow-list sanitization runs later and fails closed.
fn has_svg_root(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head);
    let lower = text.to_ascii_lowercase();
    lower.contains("<svg")
}

/// Validate an original (client-supplied) filename: strip any directory
/// component, reject separators / control characters / reserved names.
pub fn clean_filename(raw: &str) -> Option<String> {
    // Take the final path component of whatever the client sent.
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("");
    let base = base.trim();
    if base.is_empty() || base.len() > 255 {
        return None;
    }
    if base.bytes().any(|b| b < 0x20 || b == 0x7F) {
        return None;
    }
    if base == "." || base == ".." {
        return None;
    }
    // Windows reserved device names (CON, NUL, COM1, …).
    let stem = base.split('.').next().unwrap_or("");
    let upper = stem.to_ascii_uppercase();
    const RESERVED: [&str; 22] = [
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED.contains(&upper.as_str()) {
        return None;
    }
    Some(base.to_string())
}

/// Extract a lowercase, normalized extension from a filename.
pub fn extension_of(filename: &str) -> Option<String> {
    let (stem, ext) = filename.rsplit_once('.')?;
    // Dotfiles (".hidden") have no extension — the dot belongs to the name.
    if stem.is_empty() {
        return None;
    }
    let ext = ext.trim().to_ascii_lowercase();
    if ext.is_empty() || ext.len() > 16 || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext)
}

/// The fully validated upload: content-derived type and limits.
#[derive(Clone, Debug)]
pub struct ValidatedUpload {
    pub file_type: FileType,
    /// Sanitized original filename (display only).
    pub filename: String,
    /// Lowercase extension of the (cleaned) filename.
    pub extension: String,
    /// MIME type from content sniffing (served to visitors).
    pub mime: &'static str,
    pub kind: MediaKind,
    /// Effective size cap for this kind (bytes).
    pub size_limit: u64,
}

/// Validate an upload against the whitelist and size limits.
///
/// - `filename` is the client-supplied name.
/// - `head` is the first bytes of the content (>= 512 bytes when available).
/// - `size` is the total upload size.
/// - `declared_mime` is the multipart `Content-Type` (informational only).
pub fn validate(
    cfg: &MediaConfig,
    filename: &str,
    head: &[u8],
    size: u64,
    declared_mime: &str,
) -> crate::error::AppResult<ValidatedUpload> {
    let filename = clean_filename(filename)
        .ok_or_else(|| crate::error::AppError::BadRequest("invalid filename".into()))?;
    let ext = extension_of(&filename).ok_or_else(|| {
        crate::error::AppError::BadRequest("filename has no recognizable extension".into())
    })?;

    let sniffed = sniff(head);
    let file_type = resolve_type(cfg, &ext, sniffed, head)?;
    let v = ValidatedUpload {
        file_type: file_type.clone(),
        filename,
        extension: ext,
        mime: file_type.mime,
        kind: file_type.kind,
        size_limit: cfg.upload.limits.for_kind(file_type.kind.as_str()),
    };
    if size > v.size_limit {
        return Err(crate::error::AppError::PayloadTooLarge(format!(
            "file exceeds the {} size limit ({} > {} bytes)",
            v.kind.as_str(),
            size,
            v.size_limit
        )));
    }
    let _ = declared_mime; // never trusted; sniffing decides
    Ok(v)
}

/// Extension → whitelisted type, cross-checked against sniffed content.
/// Unknown extensions are rejected even when the declared MIME looks fine
/// (`evil.php` renamed to `evil.jpg` fails the JPEG magic check).
fn resolve_type(
    cfg: &MediaConfig,
    ext: &str,
    sniffed: Sniffed,
    head: &[u8],
) -> crate::error::AppResult<FileType> {
    let whitelisted = BUILTIN_TYPES.iter().find(|t| t.extension == ext);

    let Some(t) = whitelisted else {
        // Admin-added extension: accepted without magic-byte verification and
        // always served as an attachment (octet-stream).
        let extra = cfg
            .upload
            .extra_types
            .iter()
            .any(|e| e.trim().eq_ignore_ascii_case(ext));
        if !extra {
            return Err(crate::error::AppError::BadRequest(format!(
                "file type '.{ext}' is not allowed"
            )));
        }
        return Ok(FileType {
            extension: Cow::Owned(ext.to_string()),
            mime: "application/octet-stream",
            kind: MediaKind::Other,
        });
    };

    match sniffed {
        Sniffed::Known(s) if s.extension == t.extension || compatible(s, t) => Ok(t.clone()),
        Sniffed::Known(_) => Err(crate::error::AppError::BadRequest(format!(
            "content does not match the '.{}' extension",
            t.extension
        ))),
        Sniffed::Text => {
            // Text content is accepted for text types — and for SVG, whose
            // root element must be visible in the head (the sanitizer later
            // enforces the allow-list and fails closed on anything else).
            if t.mime.starts_with("text/") || (t.extension == "svg" && has_svg_root(head)) {
                Ok(t.clone())
            } else {
                Err(crate::error::AppError::BadRequest(format!(
                    "content does not match the '.{}' extension",
                    t.extension
                )))
            }
        }
        Sniffed::Unknown => Err(crate::error::AppError::BadRequest(format!(
            "unrecognized file content for '.{}'",
            t.extension
        ))),
    }
}

/// Sniffed type vs whitelisted type equivalence for aliases (jpg/jpeg,
/// ogg/oga, tgz/gz, m4v/mp4).
fn compatible(sniffed: &FileType, wanted: &FileType) -> bool {
    matches!(
        (sniffed.extension.as_ref(), wanted.extension.as_ref()),
        ("jpg", "jpeg") | ("mp4", "m4v") | ("ogg", "oga") | ("gz", "tgz") | ("mp3", "mp3")
    ) && sniffed.kind == wanted.kind
}

// ---------------------------------------------------------------------------
// SVG sanitizer
// ---------------------------------------------------------------------------

/// Sanitize an SVG with a strict allow-list: unknown elements/attributes are
/// dropped, `script`/event handlers/external references never survive. The
/// output is re-serialized from parsed events, so obfuscated payloads
/// (entities, nested encodings) cannot pass through unexamined.
///
/// Returns `None` when the input is not parseable XML — the caller then
/// rejects the upload (fail closed).
pub fn sanitize_svg(cfg: &MediaSvgConfig, input: &[u8]) -> Option<Vec<u8>> {
    if !cfg.enabled {
        return None;
    }

    use quick_xml::Reader;
    use quick_xml::Writer;
    use quick_xml::events::Event;

    let mut reader = Reader::from_reader(input);
    reader.config_mut().trim_text(false);
    let mut writer = Writer::new(Vec::new());
    let mut depth: usize = 0;
    let mut saw_root = false;

    loop {
        let Ok(ev) = reader.read_event() else {
            return None;
        };
        match ev {
            Event::Start(e) => {
                let name = e.name();
                let tag = name.local_name();
                let tag = String::from_utf8_lossy(tag.as_ref()).into_owned();
                if depth == 0 && !is_allowed_svg_element(&tag, true) {
                    return None; // root must be <svg>
                }
                if depth > 0 && !is_allowed_svg_element(&tag, false) {
                    // Skip the disallowed subtree entirely.
                    skip_subtree(&mut reader)?;
                    continue;
                }
                saw_root = true;
                writer.write_event(Event::Start(clean_attrs(&e))).ok()?;
                depth += 1;
            }
            Event::Empty(e) => {
                let tag = String::from_utf8_lossy(e.name().local_name().as_ref()).into_owned();
                if !is_allowed_svg_element(&tag, depth == 0) {
                    continue;
                }
                saw_root = true;
                writer.write_event(Event::Empty(clean_attrs(&e))).ok()?;
            }
            Event::End(_) => {
                depth = depth.saturating_sub(1);
                writer.write_event(ev).ok()?;
            }
            Event::Text(t) => {
                // Text content is escaped by the writer — no markup smuggling.
                writer.write_event(Event::Text(t)).ok()?;
            }
            Event::CData(_) | Event::Comment(_) | Event::DocType(_) | Event::Decl(_) => {
                // Comments can carry IE conditionals; CDATA can hide payloads
                // from naive regex filters — dropped wholesale.
                if matches!(ev, Event::Decl(_)) {
                    continue; // decl is harmless but unnecessary
                }
                continue;
            }
            Event::PI(_) => continue,
            Event::Eof => break,
        }
    }
    if !saw_root {
        return None;
    }
    Some(writer.into_inner())
}

/// Skip all events until the subtree of the already-consumed `Start` element
/// closes (depth starts at 1: the skipped element itself).
fn skip_subtree(reader: &mut quick_xml::Reader<&[u8]>) -> Option<()> {
    use quick_xml::events::Event;
    let mut depth: usize = 1;
    loop {
        let ev = reader.read_event().ok()?;
        match ev {
            Event::Start(_) => depth += 1,
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    return Some(());
                }
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

/// Elements allowed in sanitized SVG. Vector primitives, grouping and text
/// only — no `script`, `foreignObject`, `use` (external refs), `style`
/// (CSS-based exfiltration), filters referencing external resources.
fn is_allowed_svg_element(tag: &str, is_root: bool) -> bool {
    if is_root {
        return tag == "svg";
    }
    matches!(
        tag,
        "g" | "defs"
            | "title"
            | "desc"
            | "metadata"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "textPath"
            | "linearGradient"
            | "radialGradient"
            | "stop"
            | "clipPath"
            | "mask"
            | "pattern"
            | "marker"
            | "symbol"
            | "view"
            | "switch"
            | "a"
            | "image"
    )
}

/// Attributes whose values may contain `url(...)` references. Only internal
/// anchors survive — an external `url()` would turn the sanitized SVG into
/// a cross-origin fetch beacon as soon as the CSP is relaxed.
const URL_REF_ATTRS: &[&str] = &[
    "fill",
    "stroke",
    "clip-path",
    "mask",
    "marker-end",
    "marker-mid",
    "marker-start",
];

/// True when every `url(...)` in the value references the document itself
/// (`url(#id)`, optionally quoted); values without any `url(` pass untouched.
fn is_internal_url_ref(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let mut from = 0usize;
    while let Some(pos) = lower[from..].find("url(") {
        let start = from + pos + "url(".len();
        let rest = lower[start..].trim_start();
        let rest = rest.strip_prefix(['\'', '"']).unwrap_or(rest);
        if !rest.starts_with('#') {
            return false;
        }
        from = start;
    }
    true
}

/// Copy an element, keeping only allow-listed attributes. `href` values must
/// be internal fragments (`#id`); everything else is dropped. Returns an
/// owned event (the reader buffer is released immediately).
fn clean_attrs(e: &quick_xml::events::BytesStart<'_>) -> quick_xml::events::BytesStart<'static> {
    let name = String::from_utf8_lossy(e.name().as_ref()).into_owned();
    let mut cleaned = quick_xml::events::BytesStart::new(&name).into_owned();
    for attr in e.attributes().with_checks(true).flatten() {
        let key = String::from_utf8_lossy(attr.key.as_ref()).into_owned();
        let lower = key.to_ascii_lowercase();
        let value = String::from_utf8_lossy(&attr.value).into_owned();

        // Event handlers and style are never allowed.
        if lower.starts_with("on") || lower == "style" {
            continue;
        }
        // href / xlink:href: internal references only.
        if lower == "href" || lower == "xlink:href" || lower.ends_with(":href") {
            if value.trim().starts_with('#') {
                cleaned.push_attribute((key.as_str(), value.as_str()));
            }
            continue;
        }
        if is_allowed_svg_attr(&lower) {
            if URL_REF_ATTRS.contains(&lower.as_str()) && !is_internal_url_ref(&value) {
                continue;
            }
            cleaned.push_attribute((key.as_str(), value.as_str()));
        }
    }
    cleaned
}

const ALLOWED_ATTRS: &[&str] = &[
    "id",
    "class",
    "d",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "width",
    "height",
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "opacity",
    "transform",
    "transform-origin",
    "viewbox",
    "preserveaspectratio",
    "points",
    "offset",
    "stop-color",
    "stop-opacity",
    "gradientunits",
    "gradienttransform",
    "patternunits",
    "patterncontentunits",
    "patterntransform",
    "text-anchor",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "dominant-baseline",
    "xml:space",
    "xmlns",
    "xmlns:xlink",
    "version",
    "clip-path",
    "clip-rule",
    "mask",
    "marker-end",
    "marker-mid",
    "marker-start",
    "markerwidth",
    "markerheight",
    "refx",
    "refy",
    "orient",
    "dx",
    "dy",
    "rotate",
    "textlength",
    "lengthadjust",
    "spreadmethod",
    "fx",
    "fy",
    "patterncontentunits",
    "requiredfeatures",
    "systemlanguage",
    "target",
    "role",
    "aria-label",
    "aria-hidden",
    "aria-labelledby",
    "aria-describedby",
    "focusable",
    "tabindex",
];

fn is_allowed_svg_attr(lower: &str) -> bool {
    ALLOWED_ATTRS.contains(&lower)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MediaConfig;

    fn cfg() -> MediaConfig {
        MediaConfig::default()
    }

    fn validate_ok(name: &str, head: &[u8], size: u64) -> ValidatedUpload {
        validate(&cfg(), name, head, size, "").unwrap()
    }

    #[test]
    fn accepts_real_jpeg() {
        let v = validate_ok("photo.JPG", &[0xFF, 0xD8, 0xFF, 0xE0], 100);
        assert_eq!(v.mime, "image/jpeg");
        assert_eq!(v.kind, MediaKind::Image);
    }

    #[test]
    fn rejects_php_disguised_as_jpg() {
        let err = validate(
            &cfg(),
            "evil.jpg",
            b"<?php system($_GET['c']); ?>",
            20,
            "image/jpeg",
        )
        .unwrap_err();
        assert!(err.message().contains("does not match"));
    }

    #[test]
    fn rejects_unknown_extension() {
        let err = validate(&cfg(), "shell.php", b"<?php echo 1;", 20, "").unwrap_err();
        assert!(err.message().contains("not allowed"));
    }

    #[test]
    fn admin_extra_types_are_accepted() {
        let mut c = cfg();
        c.upload.extra_types = vec!["docx".into()];
        let v = validate(&c, "report.docx", b"PK\x03\x04fake", 20, "").unwrap();
        assert_eq!(v.mime, "application/octet-stream");
        // Not whitelisted for real: still rejected when not configured.
        assert!(validate(&cfg(), "report.docx", b"PK\x03\x04fake", 20, "").is_err());
    }

    #[test]
    fn rejects_oversized_by_kind() {
        let err =
            validate(&cfg(), "big.jpg", &[0xFF, 0xD8, 0xFF], 21 * 1024 * 1024, "").unwrap_err();
        assert!(err.message().contains("exceeds"));
    }

    #[test]
    fn filename_cleaning() {
        assert_eq!(clean_filename("/etc/passwd").as_deref(), Some("passwd"));
        assert_eq!(
            clean_filename("C:\\Users\\x\\a.png").as_deref(),
            Some("a.png")
        );
        assert_eq!(clean_filename(".."), None);
        assert_eq!(clean_filename("CON.txt"), None);
        assert_eq!(clean_filename("ok\u{1}.png"), None);
        assert_eq!(
            clean_filename("  spaced.png  ").as_deref(),
            Some("spaced.png")
        );
    }

    #[test]
    fn extension_extraction() {
        assert_eq!(extension_of("a.tar.gz").as_deref(), Some("gz"));
        assert!(extension_of("noext").is_none());
        assert!(extension_of(".hidden").is_none());
        assert_eq!(extension_of("a.zip.exe").as_deref(), Some("exe"));
    }

    #[test]
    fn tar_detection() {
        let mut head = vec![0u8; 512];
        head[257..262].copy_from_slice(b"ustar");
        let v = validate_ok("backup.tar", &head, 4096);
        assert_eq!(v.kind, MediaKind::Archive);
    }

    #[test]
    fn svg_sanitizer_strips_scripts() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)">
            <script>alert(2)</script>
            <rect width="10" height="10" fill="red" onclick="x()"/>
            <a href="https://evil.example"><circle r="5"/></a>
            <image href="#ok"/>
        </svg>"##;
        let cfg = MediaSvgConfig::default();
        let out = sanitize_svg(&cfg, svg).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("script"), "script must be removed: {s}");
        assert!(!s.contains("onload"), "handlers must be removed: {s}");
        assert!(!s.contains("onclick"), "handlers must be removed: {s}");
        assert!(
            !s.contains("evil.example"),
            "external href must be removed: {s}"
        );
        assert!(s.contains("rect"), "safe elements must survive: {s}");
        assert!(s.contains("circle"), "safe elements must survive: {s}");
        assert!(s.contains("href=\"#ok\""), "internal refs survive: {s}");
    }

    #[test]
    fn svg_sanitizer_rejects_broken_markup() {
        let cfg = MediaSvgConfig::default();
        assert!(sanitize_svg(&cfg, b"<svg><rect").is_none());
        assert!(sanitize_svg(&cfg, b"not xml at all").is_none());
    }

    #[test]
    fn svg_sanitizer_requires_svg_root() {
        let cfg = MediaSvgConfig::default();
        assert!(sanitize_svg(&cfg, b"<html><body>x</body></html>").is_none());
    }

    #[test]
    fn svg_sanitizer_drops_style_and_comments() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg">
            <style>rect { fill: url(https://evil) }</style>
            <!-- ie conditional junk -->
            <rect width="1" height="1"/>
        </svg>"#;
        let cfg = MediaSvgConfig::default();
        let out = sanitize_svg(&cfg, svg).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(!s.contains("style"), "style must be dropped: {s}");
        assert!(
            !s.contains("ie conditional"),
            "comments must be dropped: {s}"
        );
    }

    #[test]
    fn svg_sanitizer_blocks_external_url_references() {
        let cfg = MediaSvgConfig::default();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg">
            <rect width="10" height="10" fill="url(https://evil.example/track)"/>
            <circle r="5" stroke="URL('//evil.example/x')"/>
            <ellipse rx="4" fill="url(#g1)"/>
        </svg>"#;
        let out = sanitize_svg(&cfg, svg).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(
            !s.contains("evil.example"),
            "external url() must be dropped: {s}"
        );
        assert!(s.contains("url(#g1)"), "internal url() survives: {s}");
    }
}
