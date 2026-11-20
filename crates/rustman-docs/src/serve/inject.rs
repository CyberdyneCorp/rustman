//! What the preview adds to the HTML it serves: the live-reload script and,
//! after a failed build, the error overlay. Only served responses change;
//! the built files never contain either.

use crate::html::escape_html;

use super::RELOAD_PATH;

/// `html` with the reload script (and the overlay for `error`) inserted
/// before its last `</body>`, or appended when it has none.
pub fn inject(html: &str, error: Option<&str>, generation: u64) -> String {
    let mut extra = String::new();
    if let Some(error) = error {
        extra.push_str(&overlay(error));
    }
    extra.push_str(&script(generation));
    let at = html
        .to_ascii_lowercase()
        .rfind("</body>")
        .unwrap_or(html.len());
    let mut out = String::with_capacity(html.len() + extra.len());
    out.push_str(&html[..at]);
    out.push_str(&extra);
    out.push_str(&html[at..]);
    out
}

/// A page for a path the current build does not have, or for when no build
/// succeeded yet; it reloads after the next build.
pub fn placeholder(title: &str, error: Option<&str>, generation: u64) -> String {
    let page = format!(
        "<!doctype html>\n<html lang=\"en\">\n<head><meta charset=\"utf-8\"><title>{0}</title></head>\n<body>\n<p>{0}</p>\n</body>\n</html>\n",
        escape_html(title)
    );
    inject(&page, error, generation)
}

/// The script reloading the page once a build newer than `generation`
/// finished. It is inline, so the page still makes no outside request.
fn script(generation: u64) -> String {
    format!(
        "<script>(function(){{var g=\"{generation}\";var s=new EventSource(\"{RELOAD_PATH}\");s.onmessage=function(e){{if(e.data!==g){{s.close();location.reload()}}}}}})();</script>\n"
    )
}

/// The build error over the page (white on dark red: 9.6:1).
fn overlay(error: &str) -> String {
    format!(
        "<div id=\"rustman-preview-error\" role=\"alert\" style=\"position:fixed;left:0;right:0;bottom:0;max-height:50vh;overflow:auto;margin:0;padding:12px 16px;background:#7f1d1d;color:#ffffff;font:13px/1.4 monospace;z-index:2147483647\"><strong>docs build failed; showing the last good build</strong><pre style=\"white-space:pre-wrap;margin:8px 0 0\">{}</pre></div>\n",
        escape_html(error)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_script_goes_before_the_last_body_end() {
        let html = "<html><body><p>a</p></BODY></html>\n";
        let out = inject(html, None, 3);
        assert!(out.starts_with("<html><body><p>a</p><script>"));
        assert!(out.contains("var g=\"3\""));
        assert!(out.contains(RELOAD_PATH));
        assert!(out.ends_with("</script>\n</BODY></html>\n"));
        assert!(!out.contains("rustman-preview-error"));
    }

    #[test]
    fn a_page_without_body_gets_the_script_at_the_end() {
        let out = inject("plain", None, 1);
        assert!(out.starts_with("plain<script>"));
    }

    #[test]
    fn the_overlay_escapes_the_error() {
        let out = inject(
            "<body></body>",
            Some("bad <script>alert(1)</script> & \"x\""),
            2,
        );
        assert!(out.contains("rustman-preview-error"));
        assert!(out.contains("bad &lt;script&gt;alert(1)&lt;/script&gt; &amp; &quot;x&quot;"));
        assert!(!out.contains("<script>alert(1)"));
    }

    #[test]
    fn placeholders_escape_their_title() {
        let out = placeholder("<b>", None, 0);
        assert!(out.contains("<title>&lt;b&gt;</title>"));
        assert!(out.contains("EventSource"));
    }
}
