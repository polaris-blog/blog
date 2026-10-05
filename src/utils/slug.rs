/// Generate a URL slug from a title. Keeps unicode alphanumerics (so CJK
/// titles produce readable, URL-encodable slugs), lowercases ASCII, and
/// collapses separators.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for c in input.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_dash = false;
        } else if (c == '-' || c == '_' || c == ' ') && !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Fallback when a title produces no usable characters.
pub fn fallback_slug(prefix: &str) -> String {
    format!("{}-{}", prefix, crate::utils::time::now())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic() {
        assert_eq!(slugify("Hello World!"), "hello-world");
        assert_eq!(slugify("  Multiple   Spaces  "), "multiple-spaces");
        assert_eq!(slugify("A_B-C"), "a-b-c");
        assert_eq!(slugify("Тест"), "тест");
        assert_eq!(slugify("你好世界"), "你好世界");
        assert_eq!(slugify("!!!"), "");
        assert_eq!(slugify("trailing---dashes---"), "trailing-dashes");
    }
}
