//! The post-build leak scan (design D3): a second line of defense that
//! looks for every known secret value in every output file, in the forms
//! the core redactor matches (raw, JSON and HTML escapes, URL encoding,
//! base64 and hex), whatever template produced it.

use crate::output::Files;
use crate::rendered::DocsRedactor;

/// The files of `files` that hold a known secret value, in path order.
pub fn scan(files: &Files, redactor: &DocsRedactor) -> Vec<String> {
    files
        .iter()
        .filter(|(_, bytes)| redactor.finds_secret(&String::from_utf8_lossy(bytes)))
        .map(|(name, _)| name.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustman_core::redact::base64_forms;
    use rustman_core::{Redactor, Secret};

    const VALUE: &str = "CANARY s3cr3t/+&=";

    fn redactor() -> DocsRedactor {
        DocsRedactor::new(Redactor::new().with(&Secret::new("token", VALUE.to_owned())))
    }

    #[test]
    fn plain_base64_and_url_encoded_forms_are_found() {
        let mut files = Files::new();
        files.insert(
            "clean.html".into(),
            b"<p>\xc2\xabredacted:token\xc2\xbb</p>".to_vec(),
        );
        files.insert("plain.html".into(), format!("<p>{VALUE}</p>").into_bytes());
        files.insert("b64.js".into(), base64_forms(VALUE)[3].clone().into_bytes());
        files.insert(
            "url.html".into(),
            b"?q=CANARY%20s3cr3t%2F%2B%26%3D".to_vec(),
        );
        files.insert("html.html".into(), b"CANARY s3cr3t/+&amp;=".to_vec());
        assert_eq!(
            scan(&files, &redactor()),
            ["b64.js", "html.html", "plain.html", "url.html"]
        );
    }
}
