//! Search query types and user-input normalization.
//!
//! User input never reaches SQL verbatim: it goes through
//! `normalize → validate → parse`, producing dialect-safe strings
//! (quoted phrases for FTS5, sanitized lexemes for tsquery, stripped
//! terms for MySQL boolean mode). See [`ParsedQuery`].

use crate::error::{AppError, AppResult};

/// Result ordering.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchSort {
    #[default]
    Relevance,
    Date,
    Updated,
    Title,
}

impl SearchSort {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "relevance" | "" => Some(Self::Relevance),
            "date" => Some(Self::Date),
            "updated" => Some(Self::Updated),
            "title" => Some(Self::Title),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Date => "date",
            Self::Updated => "updated",
            Self::Title => "title",
        }
    }
}

/// What to search: posts, pages, media, or all public content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchKind {
    Post,
    Page,
    /// Media library items (managed via the media search API; excluded from
    /// public site search — see [`SearchQuery::include_hidden`]).
    Media,
}

impl SearchKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "post" | "posts" => Some(Self::Post),
            "page" | "pages" => Some(Self::Page),
            "media" => Some(Self::Media),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Post => "post",
            Self::Page => "page",
            Self::Media => "media",
        }
    }
}

/// A search request as handed to [`crate::search::SearchService`].
#[derive(Clone, Debug)]
pub struct SearchQuery {
    pub query: String,
    pub page: u32,
    pub per_page: u32,
    pub sort: SearchSort,
    pub kind: Option<SearchKind>,
    pub category: Option<String>,
    pub tag: Option<String>,
    pub author: Option<String>,
    /// Include rows hidden from public search (media rows are indexed with
    /// `visible = 0` so they never leak into the public search page; the
    /// media library API sets this to true).
    pub include_hidden: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            query: String::new(),
            page: 1,
            per_page: 10,
            sort: SearchSort::default(),
            kind: None,
            category: None,
            tag: None,
            author: None,
            include_hidden: false,
        }
    }
}

/// Hard upper bounds applied regardless of configuration.
const MAX_QUERY_CHARS: usize = 100;
const MAX_TERMS: usize = 8;

/// A fully parsed, dialect-safe search query.
#[derive(Clone, Debug)]
pub struct ParsedQuery {
    /// Normalized text, e.g. `"rust web"` from `"  RuST   web "`.
    pub normalized: String,
    /// Sanitized lowercase terms (no operators, no wildcards).
    pub terms: Vec<String>,
    /// FTS5 MATCH expression: `"rust"* "web"*` (quoted phrases + prefix,
    /// so user input is never interpreted as FTS syntax).
    pub fts_match: String,
    /// PostgreSQL to_tsquery expression: `rust:* & web:*`.
    pub pg_tsquery: String,
    /// MySQL boolean-mode expression: `+rust* +web*`.
    pub mysql_boolean: String,
}

impl ParsedQuery {
    /// Normalize → validate → parse. Errors on empty/too-short queries.
    pub fn parse(raw: &str, minimum_len: usize) -> AppResult<Self> {
        let normalized = normalize(raw);
        if normalized.chars().count() < minimum_len.max(1) {
            return Err(AppError::BadRequest(format!(
                "query must be at least {minimum_len} characters"
            )));
        }
        let terms = extract_terms(&normalized);
        if terms.is_empty() {
            return Err(AppError::BadRequest(
                "query contains no searchable terms".into(),
            ));
        }
        let fts_match = terms
            .iter()
            .map(|t| format!("\"{}\"*", t.replace('"', "\"\"")))
            .collect::<Vec<_>>()
            .join(" ");
        let pg_tsquery = terms
            .iter()
            .map(|t| format!("{t}:*"))
            .collect::<Vec<_>>()
            .join(" & ");
        let mysql_boolean = terms
            .iter()
            .map(|t| format!("+{t}*"))
            .collect::<Vec<_>>()
            .join(" ");
        Ok(Self {
            fts_match,
            pg_tsquery,
            mysql_boolean,
            normalized,
            terms,
        })
    }
}

/// Collapse whitespace, trim, lowercase — `"  Rust   WEB "` → `"rust web"`.
pub fn normalize(raw: &str) -> String {
    raw.chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect::<String>()
        .split(' ')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .chars()
        .take(MAX_QUERY_CHARS)
        .collect()
}

/// Split into sanitized terms: drop FTS/tsquery/boolean operators and
/// wildcard characters so nothing is ever interpreted as query syntax.
fn extract_terms(normalized: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for word in normalized.split(' ') {
        let cleaned: String = word
            .chars()
            // Strip everything that carries meaning in FTS5, tsquery or
            // MySQL boolean mode, plus SQL LIKE wildcards.
            .filter(|c| {
                !matches!(
                    c,
                    '"' | '\''
                        | '*'
                        | '^'
                        | '-'
                        | '+'
                        | '|'
                        | '&'
                        | '('
                        | ')'
                        | ':'
                        | '!'
                        | '<'
                        | '>'
                        | '~'
                        | '%'
                        | '_'
                        | ','
                        | ';'
                        | '.'
                        | '?'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                )
            })
            .collect();
        let cleaned = cleaned.trim();
        if !cleaned.is_empty() && !out.iter().any(|t| t == cleaned) {
            out.push(cleaned.to_string());
        }
        if out.len() == MAX_TERMS {
            break;
        }
    }
    out
}

/// SQL LIKE prefix pattern for suggestions: `ru%` (input pre-sanitized by
/// the caller; `%`/`_` are stripped by [`normalize`]-level term cleaning).
pub fn like_prefix(term: &str) -> String {
    let cleaned: String = term
        .chars()
        .filter(|c| !matches!(c, '%' | '_' | '\\'))
        .collect();
    format!("{}%", cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_whitespace_and_case() {
        assert_eq!(normalize("   rust   WEB  "), "rust web");
        assert_eq!(normalize(""), "");
    }

    #[test]
    fn parse_builds_dialect_safe_forms() {
        let p = ParsedQuery::parse("  Rust Web  ", 2).unwrap();
        assert_eq!(p.terms, vec!["rust", "web"]);
        assert_eq!(p.fts_match, "\"rust\"* \"web\"*");
        assert_eq!(p.pg_tsquery, "rust:* & web:*");
        assert_eq!(p.mysql_boolean, "+rust* +web*");
    }

    #[test]
    fn injection_operators_are_stripped() {
        let p = ParsedQuery::parse(r#"rust" OR 1=1; DROP"* (a & b) % _"#, 2).unwrap();
        for t in &p.terms {
            assert!(
                !t.contains(['"', '*', '&', '|', '(', ')', '%', '_', ';']),
                "term not sanitized: {t}"
            );
        }
        // FTS syntax never survives: everything is a quoted phrase.
        assert!(p.fts_match.starts_with('"'));
        assert!(p.mysql_boolean.starts_with('+'));
    }

    #[test]
    fn too_short_query_rejected() {
        assert!(ParsedQuery::parse("r", 2).is_err());
        assert!(ParsedQuery::parse("   ", 2).is_err());
    }

    #[test]
    fn caps_term_count_and_dedupes() {
        let long = "a1 b2 c3 d4 e5 f6 g7 h8 i9 j10";
        let p = ParsedQuery::parse(long, 2).unwrap();
        assert_eq!(p.terms.len(), 8);
        let p = ParsedQuery::parse("rust rust rust", 2).unwrap();
        assert_eq!(p.terms.len(), 1);
    }

    #[test]
    fn cjk_terms_pass_through() {
        let p = ParsedQuery::parse("Rust 高性能", 2).unwrap();
        assert!(p.terms.contains(&"高性能".to_string()));
        assert!(p.pg_tsquery.contains("高性能:*"));
    }

    #[test]
    fn query_length_capped() {
        let raw = "x".repeat(500);
        let p = ParsedQuery::parse(&raw, 2).unwrap();
        assert!(p.normalized.chars().count() <= 100);
    }

    #[test]
    fn sort_parsing() {
        assert_eq!(SearchSort::parse("date"), Some(SearchSort::Date));
        assert_eq!(SearchSort::parse(""), Some(SearchSort::Relevance));
        assert_eq!(SearchSort::parse("bogus"), None);
    }
}
