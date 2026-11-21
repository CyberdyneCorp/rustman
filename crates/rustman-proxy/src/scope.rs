//! The interception scope (spec: Interception scope, design D4): `CONNECT`
//! hosts are matched against the include globs before any TLS handshake;
//! everything else stays an opaque tunnel.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

/// Host globs whose tunnels are decrypted; empty decrypts nothing.
#[derive(Clone, Debug)]
pub struct Scope {
    set: GlobSet,
    empty: bool,
}

impl Scope {
    /// The scope of `patterns` (`*.example.com`, `localhost`), matched
    /// without regard to case.
    pub fn new(patterns: &[String]) -> Result<Self, String> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let glob = GlobBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("record: invalid --intercept glob {pattern:?}: {e}"))?;
            builder.add(glob);
        }
        let set = builder.build().map_err(|e| format!("record: {e}"))?;
        Ok(Self {
            set,
            empty: patterns.is_empty(),
        })
    }

    pub fn is_empty(&self) -> bool {
        self.empty
    }

    /// Whether the tunnel to `host` (no port) is decrypted.
    pub fn matches(&self, host: &str) -> bool {
        !self.empty && self.set.is_match(host)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(patterns: &[&str]) -> Scope {
        let patterns: Vec<String> = patterns.iter().map(|&p| p.to_owned()).collect();
        Scope::new(&patterns).unwrap()
    }

    #[test]
    fn hosts_match_the_include_globs() {
        let scope = scope(&["*.example.com", "localhost"]);
        assert!(scope.matches("api.example.com"));
        assert!(scope.matches("API.Example.com"));
        assert!(scope.matches("localhost"));
        assert!(!scope.matches("www.google.com"));
        assert!(!scope.matches("example.com.evil.test"));
    }

    #[test]
    fn an_empty_scope_decrypts_nothing() {
        let scope = scope(&[]);
        assert!(scope.is_empty());
        assert!(!scope.matches("api.example.com"));
    }

    #[test]
    fn invalid_globs_are_reported() {
        let err = Scope::new(&["a[".to_owned()]).unwrap_err();
        assert!(
            err.starts_with("record: invalid --intercept glob \"a[\""),
            "{err}"
        );
    }
}
