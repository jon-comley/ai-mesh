//! Reports and hubs rendered for the page.
//!
//! Raw HTML in the Markdown is shown as text, never passed through: the files
//! are written by agents, and the page is on the LAN with buttons that act.
//! Relative links point at the file on GitHub.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, html};

/// `md` as HTML. `base` is the GitHub URL of the file's folder
/// (`https://github.com/jon-comley/desk/blob/main/reports`), used for relative links.
pub fn render(md: &str, base: Option<&str>) -> String {
    let parser = Parser::new_ext(md, Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH);
    let events = parser.map(|e| match e {
        Event::Html(h) | Event::InlineHtml(h) => Event::Text(h),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: link(dest_url, base),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: link(dest_url, base),
            title,
            id,
        }),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out
}

fn link<'a>(dest: CowStr<'a>, base: Option<&str>) -> CowStr<'a> {
    let lower = dest.to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("mailto:")
    {
        return dest;
    }
    if dest.starts_with('#') {
        return dest;
    }
    // Anything else with a scheme (javascript:, data:) goes nowhere.
    if dest
        .split('/')
        .next()
        .is_some_and(|first| first.contains(':'))
    {
        return CowStr::from("#");
    }
    match base {
        Some(base) => CowStr::from(format!("{}/{}", base.trim_end_matches('/'), resolve(&dest))),
        None => dest,
    }
}

/// Folds `./` and `../` in a relative path; `..` at the top is dropped.
fn resolve(rel: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let mut ups = 0;
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    ups += 1;
                }
            }
            p => parts.push(p),
        }
    }
    let mut out = "../".repeat(ups);
    out.push_str(&parts.join("/"));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://github.com/jon-comley/desk/blob/main/reports";

    #[test]
    fn renders_headings_tables_and_lists() {
        let h = render(
            "# Report\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n- one\n- two\n",
            None,
        );
        assert!(h.contains("<h1>Report</h1>"));
        assert!(h.contains("<table>") && h.contains("<td>1</td>"));
        assert!(h.contains("<li>one</li>"));
    }

    #[test]
    fn raw_html_is_shown_as_text() {
        let h = render(
            "hi <script>alert(1)</script> <b>x</b>\n\n<div onclick=\"x\">y</div>\n",
            None,
        );
        assert!(!h.contains("<script>") && !h.contains("<b>") && !h.contains("<div"));
        assert!(h.contains("&lt;script&gt;"));
    }

    #[test]
    fn relative_links_go_to_github_and_odd_schemes_go_nowhere() {
        let h = render(
            "[r](2026-11-02.md) [hub](../HUB.md) [x](https://example.com) [j](javascript:alert(1))",
            Some(BASE),
        );
        assert!(h.contains(
            "href=\"https://github.com/jon-comley/desk/blob/main/reports/2026-11-02.md\""
        ));
        assert!(
            h.contains("href=\"https://github.com/jon-comley/desk/blob/main/reports/../HUB.md\"")
        );
        assert!(h.contains("href=\"https://example.com\""));
        assert!(h.contains("href=\"#\""));
    }

    #[test]
    fn resolve_folds_dots() {
        assert_eq!(resolve("./a/../b.md"), "b.md");
        assert_eq!(resolve("../HUB.md"), "../HUB.md");
        assert_eq!(resolve("a/./b/../c"), "a/c");
    }
}
