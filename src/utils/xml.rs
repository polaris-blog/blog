/// XML escaping for RSS / Atom / Sitemap generation.
pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn escapes() {
        assert_eq!(
            super::esc(r#"<a href="x">&'#</a>"#),
            "&lt;a href=&quot;x&quot;&gt;&amp;&apos;#&lt;/a&gt;"
        );
    }
}
