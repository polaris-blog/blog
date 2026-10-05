//! Safe Markdown rendering.
//!
//! Security model (zero extra dependencies):
//! - Raw HTML / inline HTML blocks are **dropped** (`Event::Html` / `Event::InlineHtml`).
//! - All text is escaped by pulldown-cmark itself.
//! - Link / image URLs are scheme-checked: only `http:`, `https:`, `mailto:`
//!   and scheme-relative URLs are allowed (`javascript:` / `data:` are neutralized).
//!
//! The output is therefore safe to inject into templates with `| safe`.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};

pub fn options() -> Options {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    opts
}

/// Render Markdown to sanitized HTML.
pub fn to_html(md: &str) -> String {
    let parser = Parser::new_ext(md, options()).filter_map(|ev| match ev {
        // Drop raw HTML entirely — secure by construction.
        Event::Html(_) | Event::InlineHtml(_) => None,
        Event::Start(mut tag) => {
            match &mut tag {
                Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }
                    if !url_is_safe(dest_url) =>
                {
                    *dest_url = CowStr::Borrowed("#");
                }
                _ => {}
            }
            Some(Event::Start(tag))
        }
        other => Some(other),
    });
    let mut out = String::with_capacity(md.len() / 2 + 128);
    html::push_html(&mut out, parser);
    out
}

/// Allowed URL schemes. Empty / `#` / relative URLs pass; anything with a
/// colon must use an allow-listed scheme.
pub fn url_is_safe(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.starts_with('/') || url.starts_with('#') {
        return true;
    }
    match url.split_once(':') {
        None => true, // plain relative URL
        Some((scheme, _)) => {
            let scheme = scheme.to_ascii_lowercase();
            if !scheme.chars().all(|c| c.is_ascii_alphanumeric()) {
                return false; // e.g. "java\nscript:"
            }
            matches!(scheme.as_str(), "http" | "https" | "mailto")
        }
    }
}

/// Best-effort plain text extraction from rendered HTML — used for
/// auto-summaries and RSS descriptions.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut chars = html.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' if !in_tag => {
                // Decode a handful of common entities.
                let mut entity = String::new();
                let mut consumed = 0;
                while let Some(&next) = chars.peek() {
                    if next == ';' || consumed > 8 {
                        break;
                    }
                    entity.push(next);
                    chars.next();
                    consumed += 1;
                }
                if chars.peek() == Some(&';') {
                    chars.next();
                }
                match entity.as_str() {
                    "amp" => out.push('&'),
                    "lt" => out.push('<'),
                    "gt" => out.push('>'),
                    "quot" => out.push('"'),
                    "apos" | "#39" => out.push('\''),
                    "nbsp" => out.push(' '),
                    _ => {
                        out.push('&');
                        out.push_str(&entity);
                        if chars.peek() == Some(&';') {
                            chars.next();
                            out.push(';');
                        }
                    }
                }
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// Truncate a string to at most `max_chars` characters (unicode aware),
/// cutting at a word boundary when possible.
pub fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max_chars).collect();
    if let Some(pos) = out.rfind(|c: char| c.is_whitespace())
        && pos > max_chars / 2
    {
        out.truncate(pos);
    }
    while out.ends_with(|c: char| c.is_whitespace() || c == ',' || c == '.') {
        out.pop();
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_render() {
        let html = to_html("# Hello\n\n**bold** and `code`\n\n[link](https://example.com)");
        assert!(html.contains("<h1>Hello</h1>"));
        assert!(html.contains("<strong>bold</strong>"));
        assert!(html.contains(r#"href="https://example.com""#));
    }

    #[test]
    fn raw_html_dropped() {
        let html = to_html("<script>alert(1)</script>\n\nhello <b>world</b>");
        assert!(!html.contains("<script>"));
        assert!(!html.contains("<b>"));
        assert!(html.contains("hello"));
        assert!(!html.contains("alert(1)")); // script blocks are dropped entirely
    }

    #[test]
    fn javascript_url_neutralized() {
        let html = to_html("[click](javascript:alert(1))");
        assert!(!html.contains("javascript:"));
        assert!(html.contains(r##"href="#""##));
    }

    #[test]
    fn data_url_neutralized() {
        let html = to_html("![x](data:text/html;base64,PHNjcmlwdD4=)");
        assert!(!html.contains("data:"));
    }

    #[test]
    fn scheme_tricks_blocked() {
        assert!(!url_is_safe("java\nscript:alert(1)"));
        assert!(!url_is_safe("JavaScript:alert(1)"));
        assert!(!url_is_safe("vbscript:x"));
        assert!(url_is_safe("https://ok.example"));
        assert!(url_is_safe("mailto:a@b.c"));
        assert!(url_is_safe("/relative"));
        assert!(url_is_safe("#anchor"));
        assert!(url_is_safe("page/relative"));
    }

    #[test]
    fn code_blocks_render() {
        let html = to_html("```rust\nfn main() {}\n```");
        assert!(html.contains("<pre><code"));
        assert!(html.contains("fn main() {}"));
    }

    #[test]
    fn text_extraction() {
        let text = html_to_text("<p>Hello &amp; <b>world</b>!</p>");
        assert_eq!(text, "Hello & world!");
    }

    #[test]
    fn truncation() {
        assert_eq!(truncate_chars("short", 10), "short");
        assert!(truncate_chars("hello world this is long", 11).ends_with('…'));
    }
}
