//! Error leakage (design D10): a small, versioned set of patterns for stack
//! traces, SQL error text, framework debug pages and internal file paths,
//! run over the first [`MAX_SCANNED_BYTES`] of every response body.

use std::sync::LazyLock;

use regex::Regex;

/// Bumped whenever [`PATTERNS`] changes, so reports can tell which set ran.
pub const LEAKAGE_PATTERNS_VERSION: u32 = 1;

/// Only this much of a body is scanned.
pub const MAX_SCANNED_BYTES: usize = 64 * 1024;

/// A matched excerpt is cut at this many characters.
const MAX_EXCERPT: usize = 200;

/// `(label, pattern)`, checked in order; one match per label is reported.
const PATTERNS: [(&str, &str); 16] = [
    ("Java stack trace", r"\bat [\w$.]+\([\w$]+\.java:\d+\)"),
    (
        ".NET stack trace",
        r"\bat [\w.<>`]+\([^)]*\) in \S+:line \d+",
    ),
    (".NET exception", r"\bSystem\.[A-Za-z.]*Exception\b"),
    (
        "Python traceback",
        r"Traceback \(most recent call last\)|File \x22[^\x22]+\x22, line \d+",
    ),
    (
        "Node.js stack trace",
        r"\bat [^\s()]+ \((?:node:|/|[A-Za-z]:\\)[^)]*:\d+:\d+\)",
    ),
    (
        "Go panic",
        r"goroutine \d+ \[running\]|panic: runtime error",
    ),
    ("Ruby stack trace", r"\.rb:\d+:in `"),
    ("PHP error", r"\.php on line \d+|PHP (?:Fatal|Parse) error"),
    (
        "MySQL error",
        r"You have an error in your SQL syntax|\bmysqli?_\w+\(\)|MySqlException",
    ),
    (
        "PostgreSQL error",
        r"PG::\w+Error|PSQLException|org\.postgresql\.|syntax error at or near \x22",
    ),
    ("Oracle error", r"\bORA-\d{5}\b"),
    (
        "SQL Server error",
        r"Microsoft OLE DB|ODBC SQL Server Driver|System\.Data\.SqlClient|Unclosed quotation mark",
    ),
    (
        "SQLite error",
        r"SQLite3?::\w+|sqlite3\.OperationalError|SQLITE_ERROR",
    ),
    (
        "generic SQL error",
        r"SQLSTATE\[\w+\]|unterminated quoted string",
    ),
    (
        "debug page",
        r"Whitelabel Error Page|Werkzeug Debugger|DEBUG = True|Django Version:|<title>Action Controller: Exception",
    ),
    (
        "internal file path",
        r"(?:/home/\w+|/var/www|/usr/src/app|/opt/app|[A-Za-z]:\\(?:Users|inetpub)\\)[\w./\\-]*",
    ),
];

static COMPILED: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    PATTERNS
        .iter()
        .map(|(label, pattern)| {
            (
                *label,
                Regex::new(pattern).expect("leakage patterns compile"),
            )
        })
        .collect()
});

/// Every leak in `body`: its label and the matched excerpt.
pub fn leaks(body: &[u8]) -> Vec<(&'static str, String)> {
    let cut = &body[..body.len().min(MAX_SCANNED_BYTES)];
    let text = String::from_utf8_lossy(cut);
    COMPILED
        .iter()
        .filter_map(|(label, regex)| {
            let found = regex.find(&text)?;
            Some((*label, found.as_str().chars().take(MAX_EXCERPT).collect()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(body: &str) -> Vec<&'static str> {
        leaks(body.as_bytes()).into_iter().map(|(l, _)| l).collect()
    }

    #[test]
    fn each_family_of_leak_is_found() {
        let samples = [
            (
                "at com.example.UserService.find(UserService.java:42)",
                "Java stack trace",
            ),
            (
                "   at Shop.Api.Get() in C:\\src\\Api.cs:line 12",
                ".NET stack trace",
            ),
            ("Traceback (most recent call last):", "Python traceback"),
            (
                "    at handler (/srv/app/index.js:10:5)",
                "Node.js stack trace",
            ),
            ("goroutine 1 [running]:", "Go panic"),
            ("app/models/user.rb:12:in `find'", "Ruby stack trace"),
            ("Warning: x in index.php on line 3", "PHP error"),
            (
                "You have an error in your SQL syntax; check the manual",
                "MySQL error",
            ),
            (
                "ERROR: syntax error at or near \"FROM\"",
                "PostgreSQL error",
            ),
            ("ORA-00933: SQL command not properly ended", "Oracle error"),
            (
                "Unclosed quotation mark after the character string",
                "SQL Server error",
            ),
            ("sqlite3.OperationalError: no such table", "SQLite error"),
            ("SQLSTATE[42000]: Syntax error", "generic SQL error"),
            ("<h1>Whitelabel Error Page</h1>", "debug page"),
            (
                "open /var/www/app/config.yml: no such file",
                "internal file path",
            ),
        ];
        for (body, label) in samples {
            assert!(labels(body).contains(&label), "{body}: {:?}", labels(body));
        }
    }

    #[test]
    fn the_scenario_excerpt_is_reported() {
        let found = leaks(b"oops\nat com.example.UserService.find(UserService.java:42)\n");
        assert_eq!(
            found,
            [(
                "Java stack trace",
                "at com.example.UserService.find(UserService.java:42)".to_owned()
            )]
        );
    }

    #[test]
    fn plain_errors_and_bodies_past_the_cap_are_clean() {
        assert!(labels(r#"{"error": "name is required"}"#).is_empty());
        let mut big = vec![b'a'; MAX_SCANNED_BYTES];
        big.extend_from_slice(b"Traceback (most recent call last)");
        assert!(leaks(&big).is_empty());
    }
}
