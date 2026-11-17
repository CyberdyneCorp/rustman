//! The curl options the importer knows: the supported ones, and which
//! unsupported ones take a value (so the value is skipped, not taken as a
//! URL).

/// A curl option as the importer handles it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Opt {
    Request,
    Header,
    /// `-d`, `--data`, `--data-ascii`, `--data-binary`: `@file` reads a file.
    Data,
    DataRaw,
    DataUrlencode,
    Json,
    Form,
    FormString,
    User,
    Cookie,
    Insecure,
    Location,
    Compressed,
    /// `-G`, `--get`: the data goes in the query of a `GET`.
    Get,
    Url,
    Unsupported {
        takes_value: bool,
    },
}

impl Opt {
    pub fn takes_value(self) -> bool {
        match self {
            Self::Insecure | Self::Location | Self::Compressed | Self::Get => false,
            Self::Unsupported { takes_value } => takes_value,
            _ => true,
        }
    }
}

/// Unsupported long options that take a value.
const LONG_WITH_VALUE: &[&str] = &[
    "--abstract-unix-socket",
    "--alt-svc",
    "--aws-sigv4",
    "--cacert",
    "--capath",
    "--cert",
    "--cert-type",
    "--ciphers",
    "--config",
    "--connect-timeout",
    "--connect-to",
    "--continue-at",
    "--cookie-jar",
    "--create-file-mode",
    "--crlfile",
    "--curves",
    "--data-urlencode-file",
    "--delegation",
    "--dns-interface",
    "--dns-ipv4-addr",
    "--dns-ipv6-addr",
    "--dns-servers",
    "--doh-url",
    "--dump-header",
    "--ech",
    "--engine",
    "--etag-compare",
    "--etag-save",
    "--expect100-timeout",
    "--form-escape-mode",
    "--ftp-account",
    "--ftp-alternative-to-user",
    "--ftp-method",
    "--ftp-port",
    "--ftp-ssl-ccc-mode",
    "--happy-eyeballs-timeout-ms",
    "--haproxy-clientip",
    "--hostpubmd5",
    "--hostpubsha256",
    "--hsts",
    "--interface",
    "--ip-tos",
    "--keepalive-cnt",
    "--keepalive-time",
    "--key",
    "--key-type",
    "--krb",
    "--libcurl",
    "--limit-rate",
    "--local-port",
    "--login-options",
    "--mail-auth",
    "--mail-from",
    "--mail-rcpt",
    "--max-filesize",
    "--max-redirs",
    "--max-time",
    "--netrc-file",
    "--noproxy",
    "--oauth2-bearer",
    "--output",
    "--output-dir",
    "--parallel-max",
    "--pass",
    "--pinnedpubkey",
    "--preproxy",
    "--proto",
    "--proto-default",
    "--proto-redir",
    "--proxy",
    "--proxy-cacert",
    "--proxy-cert",
    "--proxy-header",
    "--proxy-key",
    "--proxy-user",
    "--proxy1.0",
    "--pubkey",
    "--quote",
    "--random-file",
    "--range",
    "--rate",
    "--referer",
    "--request-target",
    "--resolve",
    "--retry",
    "--retry-delay",
    "--retry-max-time",
    "--sasl-authzid",
    "--service-name",
    "--socks4",
    "--socks4a",
    "--socks5",
    "--socks5-gssapi-service",
    "--socks5-hostname",
    "--speed-limit",
    "--speed-time",
    "--stderr",
    "--telnet-option",
    "--tftp-blksize",
    "--time-cond",
    "--tls-max",
    "--tls13-ciphers",
    "--tlsauthtype",
    "--tlspassword",
    "--tlsuser",
    "--trace",
    "--trace-ascii",
    "--trace-config",
    "--unix-socket",
    "--upload-file",
    "--url-query",
    "--user-agent",
    "--variable",
    "--write-out",
];

/// Unsupported short options that take a value.
const SHORT_WITH_VALUE: &str = "AcCDeEKmoPQrtTUwxyYz";

pub(super) fn long_option(name: &str) -> Opt {
    match name {
        "--request" => Opt::Request,
        "--header" => Opt::Header,
        "--data" | "--data-ascii" | "--data-binary" => Opt::Data,
        "--data-raw" => Opt::DataRaw,
        "--data-urlencode" => Opt::DataUrlencode,
        "--json" => Opt::Json,
        "--form" => Opt::Form,
        "--form-string" => Opt::FormString,
        "--user" => Opt::User,
        "--cookie" => Opt::Cookie,
        "--insecure" => Opt::Insecure,
        "--location" => Opt::Location,
        "--compressed" => Opt::Compressed,
        "--get" => Opt::Get,
        "--url" => Opt::Url,
        other => Opt::Unsupported {
            takes_value: LONG_WITH_VALUE.contains(&other),
        },
    }
}

pub(super) fn short_option(flag: char) -> Opt {
    match flag {
        'X' => Opt::Request,
        'H' => Opt::Header,
        'd' => Opt::Data,
        'F' => Opt::Form,
        'u' => Opt::User,
        'b' => Opt::Cookie,
        'k' => Opt::Insecure,
        'L' => Opt::Location,
        'G' => Opt::Get,
        other => Opt::Unsupported {
            takes_value: SHORT_WITH_VALUE.contains(other),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_lists_are_sorted_options() {
        assert!(LONG_WITH_VALUE.windows(2).all(|w| w[0] < w[1]));
        assert!(LONG_WITH_VALUE.iter().all(|o| o.starts_with("--")));
        assert_eq!(
            long_option("--limit-rate"),
            Opt::Unsupported { takes_value: true }
        );
        assert_eq!(
            long_option("--verbose"),
            Opt::Unsupported { takes_value: false }
        );
        assert_eq!(short_option('o'), Opt::Unsupported { takes_value: true });
        assert_eq!(short_option('s'), Opt::Unsupported { takes_value: false });
        assert!(!short_option('k').takes_value());
        assert!(short_option('H').takes_value());
    }
}
