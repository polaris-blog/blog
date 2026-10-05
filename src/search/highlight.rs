//! XSS-safe search highlighting.
//!
//! Produces HTML where every character of the source text is escaped and
//! matches are wrapped in `<mark>`. The only tags ever emitted are
//! `<mark>`/`</mark>`; nothing from the source text survives as markup.

/// Escape text for HTML output.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// True when `text[i..i+term.len()]` equals `term` ignoring ASCII case.
fn matches_at(text: &[char], i: usize, term: &[char]) -> bool {
    if i + term.len() > text.len() {
        return false;
    }
    text[i..i + term.len()]
        .iter()
        .zip(term)
        .all(|(a, b)| a.to_lowercase().eq(b.to_lowercase()))
}

/// Word boundary for ASCII text: non-alphanumeric neighbours. CJK text
/// has no word boundaries, so any position matches.
fn boundary_ok(text: &[char], start: usize, end: usize) -> bool {
    let is_word = |c: char| c.is_alphanumeric();
    let left_ok = start == 0 || !is_word(text[start - 1]) || !text[start].is_ascii_alphanumeric();
    let right_ok =
        end >= text.len() || !is_word(text[end]) || !text[end - 1].is_ascii_alphanumeric();
    left_ok && right_ok
}

/// Wrap term occurrences in `text` with `<mark>`, escaping everything.
pub fn highlight(text: &str, terms: &[String]) -> String {
    let chars: Vec<char> = text.chars().collect();
    let term_chars: Vec<Vec<char>> = terms.iter().map(|t| t.chars().collect()).collect();
    let mut out = String::with_capacity(text.len() + 32);
    let mut i = 0;
    while i < chars.len() {
        let mut hit: Option<usize> = None;
        for (ti, tc) in term_chars.iter().enumerate() {
            if !tc.is_empty() && matches_at(&chars, i, tc) && boundary_ok(&chars, i, i + tc.len()) {
                hit = Some(ti);
                break;
            }
        }
        match hit {
            Some(ti) => {
                let end = i + term_chars[ti].len();
                out.push_str("<mark>");
                out.push_str(&escape(&chars[i..end].iter().collect::<String>()));
                out.push_str("</mark>");
                i = end;
            }
            None => {
                out.push_str(&escape(&chars[i].to_string()));
                i += 1;
            }
        }
    }
    out
}

/// Build a snippet around the first match: a window of ~220 chars centred
/// on it, highlighted. Falls back to the head of the text.
pub fn snippet(text: &str, terms: &[String], max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return String::new();
    }
    let term_chars: Vec<Vec<char>> = terms.iter().map(|t| t.chars().collect()).collect();
    let mut first: Option<usize> = None;
    'outer: for i in 0..chars.len() {
        for tc in &term_chars {
            if !tc.is_empty() && matches_at(&chars, i, tc) {
                first = Some(i);
                break 'outer;
            }
        }
    }
    let start = match first {
        Some(i) => i.saturating_sub(60),
        None => 0,
    };
    let end = (start + max_chars).min(chars.len());
    let window: String = chars[start..end].iter().collect();
    let mut snippet = highlight(&window, terms);
    if end < chars.len() {
        snippet.push_str("&hellip;");
    }
    snippet
}

/// Does `text` contain any of `terms` (case-insensitive)?
pub fn contains_any(text: &str, terms: &[String]) -> bool {
    let chars: Vec<char> = text.chars().collect();
    let term_chars: Vec<Vec<char>> = terms.iter().map(|t| t.chars().collect()).collect();
    (0..chars.len()).any(|i| {
        term_chars
            .iter()
            .any(|tc| !tc.is_empty() && matches_at(&chars, i, tc))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn wraps_matches_in_mark() {
        let out = highlight("Rust 是一门高性能语言", &terms(&["高性能"]));
        assert_eq!(out, "Rust 是一门<mark>高性能</mark>语言");
    }

    #[test]
    fn case_insensitive() {
        let out = highlight("Rust web framework", &terms(&["rust"]));
        assert_eq!(out, "<mark>Rust</mark> web framework");
    }

    #[test]
    fn escapes_html_in_source_text() {
        let out = highlight("<script>alert('x')</script> rust", &terms(&["rust"]));
        assert_eq!(
            out,
            "&lt;script&gt;alert(&#x27;x&#x27;)&lt;/script&gt; <mark>rust</mark>"
        );
        assert!(!out.contains("<s"));
    }

    #[test]
    fn word_boundary_prevents_partial_matches() {
        let out = highlight("rustlang and rust", &terms(&["rust"]));
        // "rustlang" must NOT be marked as "rust" (boundary), plain "rust" is.
        assert_eq!(out, "rustlang and <mark>rust</mark>");
    }

    #[test]
    fn no_terms_returns_escaped_text() {
        let out = highlight("a < b", &[]);
        assert_eq!(out, "a &lt; b");
    }

    #[test]
    fn snippet_centres_on_first_match() {
        let text = format!("{} keyword {}", "x".repeat(300), "y".repeat(300));
        let out = snippet(&text, &terms(&["keyword"]), 100);
        assert!(out.contains("<mark>keyword</mark>"));
        assert!(out.ends_with("&hellip;"));
        assert!(out.chars().count() < text.chars().count());
    }

    #[test]
    fn snippet_without_match_returns_head() {
        let out = snippet("hello world", &terms(&["zzz"]), 5);
        assert_eq!(out, "hello&hellip;");
    }

    #[test]
    fn contains_any_detects_terms() {
        assert!(contains_any("Hello Rustaceans", &terms(&["rustaceans"])));
        assert!(!contains_any("Hello", &terms(&["zzz"])));
    }
}
