//! Readable text from a web page: the main content (like a browser's reader
//! view), without menus, ads and scripts.

use dom_smoothie::{Config, Readability, TextMode};

/// Parser work is bounded (very large pages are cut before, too).
const MAX_ELEMENTS: usize = 30_000;

/// Title and text of a page's main content; plain text stays as it is.
pub fn readable(html: &str, url: &str, is_html: bool) -> Option<(String, String)> {
    if !is_html {
        let text = html.trim().to_string();
        return (!text.is_empty()).then(|| (String::new(), text));
    }
    let cfg = Config {
        max_elements_to_parse: MAX_ELEMENTS,
        text_mode: TextMode::Formatted,
        ..Default::default()
    };
    let mut r = Readability::new(html, Some(url), Some(cfg)).ok()?;
    let article = r.parse().ok()?;
    let text = tidy(&article.text_content);
    (text.chars().count() >= 80).then(|| (article.title.trim().to_string(), text))
}

/// Paragraphs separated by one empty line, spaces collapsed.
fn tidy(text: &str) -> String {
    let mut out = Vec::new();
    for para in text.split("\n\n") {
        let p = para.split_whitespace().collect::<Vec<_>>().join(" ");
        if !p.is_empty() {
            out.push(p);
        }
    }
    out.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_main_content_without_menus_and_scripts() {
        let html = r#"<html><head><title>Oslo – Facts</title><script>steal()</script></head><body>
            <nav><a href="/">Home</a> <a href="/news">News</a> <a href="/about">About</a></nav>
            <article><h1>Oslo</h1>
              <p>Oslo is the capital of Norway. The municipality had 728,714 inhabitants on 1 January 2026, which makes it the most populous city in the country.</p>
              <p>The city lies at the end of the Oslofjord and is the economic and political centre of Norway, home to the parliament and the royal palace.</p>
            </article>
            <footer>Cookie settings · Imprint</footer></body></html>"#;
        let (title, text) = readable(html, "https://example.org/oslo", true).unwrap();
        assert!(title.contains("Oslo"), "{title}");
        assert!(text.contains("728,714 inhabitants"), "{text}");
        assert!(!text.contains("steal"), "{text}");
        assert!(!text.contains("Cookie settings"), "{text}");
    }

    #[test]
    fn plain_text_stays_and_empty_pages_give_nothing() {
        assert_eq!(
            readable("  just text \n", "https://e.org/a.txt", false)
                .unwrap()
                .1,
            "just text"
        );
        assert!(
            readable(
                "<html><body><p>hi</p></body></html>",
                "https://e.org/",
                true
            )
            .is_none()
        );
    }
}
