//! Comment rendering: everything escaped, then a small allow-list — **bold**, *italic*,
//! `code`, and [text](url) shown as its text only (review-ui §4.6, §10.3).

use regex::Regex;
use std::sync::OnceLock;

pub fn render(src: &str) -> String {
    static RES: OnceLock<[Regex; 4]> = OnceLock::new();
    let [code, bold, italic, link] = RES.get_or_init(|| {
        [
            Regex::new(r"`([^`\n]+)`").unwrap(),
            Regex::new(r"\*\*([^*\n]+)\*\*").unwrap(),
            Regex::new(r"\*([^*\n]+)\*").unwrap(),
            Regex::new(r"\[([^\]\n]+)\]\((?:[^()\n]|\([^()\n]*\))*\)").unwrap(),
        ]
    });
    let escaped = maud::html! { (src) }.into_string();
    let s = link.replace_all(&escaped, "$1");
    let s = code.replace_all(&s, "<code>$1</code>");
    let s = bold.replace_all(&s, "<strong>$1</strong>");
    let s = italic.replace_all(&s, "<em>$1</em>");
    s.replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_then_formats() {
        assert_eq!(render("<script>alert(1)</script>"), "&lt;script&gt;alert(1)&lt;/script&gt;");
        assert_eq!(render("**b** *i* `c<`"), "<strong>b</strong> <em>i</em> <code>c&lt;</code>");
        assert_eq!(render("[click](javascript:alert(1))"), "click");
    }
}
