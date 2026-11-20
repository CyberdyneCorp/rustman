//! CommonMark descriptions to safe HTML.
//!
//! Raw HTML in a description is shown as text (an HTML block as a
//! paragraph, so the output is always a sequence of HTML blocks and never
//! bare text a Markdown renderer would parse again), link targets other
//! than `http(s)`, `mailto`, fragments and relative paths become `#`, and
//! images become links, so a page never loads anything from the network.

use pulldown_cmark::{CowStr, Event, Options, Parser, Tag, TagEnd, html};
use serde::{Serialize, Serializer};

use crate::rendered::Rendered;

/// HTML made from redacted Markdown by [`to_html`]: safe to insert as is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Html(String);

impl Html {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for Html {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// `markdown` rendered as HTML, with raw HTML escaped and links checked.
pub fn to_html(markdown: &Rendered) -> Html {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let events = Parser::new_ext(markdown.as_str(), options).map(safe_event);
    let mut out = String::new();
    html::push_html(&mut out, events);
    Html(out)
}

fn safe_event(event: Event<'_>) -> Event<'_> {
    match event {
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        Event::Start(Tag::HtmlBlock) => Event::Start(Tag::Paragraph),
        Event::End(TagEnd::HtmlBlock) => Event::End(TagEnd::Paragraph),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        })
        | Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        Event::End(TagEnd::Image) => Event::End(TagEnd::Link),
        other => other,
    }
}

/// `url` when its scheme is allowed, else `#`.
fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    let lower = url.trim().to_ascii_lowercase();
    let scheme_end = lower.find(':');
    let path_start = lower.find(['/', '?', '#']).unwrap_or(lower.len());
    let allowed = match scheme_end {
        Some(end) if end < path_start => ["http", "https", "mailto"].contains(&&lower[..end]),
        _ => true,
    };
    if allowed { url } else { CowStr::Borrowed("#") }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rendered::DocsRedactor;

    fn html(markdown: &str) -> String {
        to_html(&DocsRedactor::default().render(markdown)).0
    }

    #[test]
    fn markdown_becomes_html() {
        assert_eq!(
            html("Lists *active* users."),
            "<p>Lists <em>active</em> users.</p>\n"
        );
        assert!(html("| a |\n|---|\n| 1 |\n").contains("<table>"));
    }

    #[test]
    fn raw_html_is_escaped() {
        let out = html("<script>alert(1)</script>\n\nhi <img src=x onerror=alert(1)>");
        assert!(!out.contains("<script") && !out.contains("<img"), "{out}");
        assert!(out.contains("&lt;script&gt;"), "{out}");
    }

    #[test]
    fn html_blocks_become_paragraphs() {
        let out = html("<script>alert(3)</script> [x](javascript:alert(4))\n\nnext");
        assert!(
            out.starts_with("<p>&lt;script&gt;alert(3)&lt;/script&gt; [x](javascript:alert(4))"),
            "{out}"
        );
        assert!(out.contains("</p>\n<p>next</p>"), "{out}");
    }

    #[test]
    fn unsafe_links_and_images_are_neutralized() {
        let out = html("[a](javascript:alert(1)) [b](https://ok.example/) [c](#x) [d](a/b:c)");
        assert!(out.contains("href=\"#\""), "{out}");
        assert!(out.contains("href=\"https://ok.example/\""), "{out}");
        assert!(
            out.contains("href=\"#x\"") && out.contains("href=\"a/b:c\""),
            "{out}"
        );
        let image = html("![logo](https://cdn.example/x.png)");
        assert!(
            !image.contains("<img") && !image.contains("src="),
            "{image}"
        );
        assert!(
            image.contains("<a href=\"https://cdn.example/x.png\">logo</a>"),
            "{image}"
        );
        assert!(html("[x](VBScript:y)").contains("href=\"#\""));
    }
}
