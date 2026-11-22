//! `Secret<T>`: a value that must never reach an output unredacted (design D7).

use std::fmt;

/// A named secret value.
///
/// It has no `Display` and no `Serialize`, and its `Debug` prints
/// `«redacted:<name>»`, so formatting or serializing a structure that holds a
/// secret cannot leak it. The value is only reachable through
/// [`Secret::expose`], which the transport writer calls when it builds the
/// bytes that go on the wire.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret<T> {
    name: String,
    value: T,
}

impl<T> Secret<T> {
    pub fn new(name: impl Into<String>, value: T) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    /// The variable name the secret is known by.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The real value. Only the transport and the secret store may call this.
    pub fn expose(&self) -> &T {
        &self.value
    }

    /// The real value, moved out (into a zeroizing holder, say) so no
    /// copy of it is left behind.
    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", placeholder(&self.name))
    }
}

/// A field whose value `Debug` never shows; it prints `«redacted:<field>»`.
pub(crate) struct Hidden(pub &'static str);

impl fmt::Debug for Hidden {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&placeholder(self.0))
    }
}

/// The text that replaces a redacted value: `«redacted:<name>»`.
pub fn placeholder(name: &str) -> String {
    format!("«redacted:{name}»")
}

/// Every `«redacted:...»` placeholder in `text`, concatenated: what a
/// redacted credential is shown as, without the parts that were not
/// recognized as secrets.
pub fn placeholders(text: &str) -> String {
    const START: &str = "«redacted:";
    let mut found = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(START) {
        let tail = &rest[start..];
        let end = tail.find('»').map_or(tail.len(), |e| e + '»'.len_utf8());
        found.push_str(&tail[..end]);
        rest = &tail[end..];
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_collected_in_order() {
        assert_eq!(
            placeholders("Bearer «redacted:a»x«redacted:b»"),
            "«redacted:a»«redacted:b»"
        );
        assert_eq!(placeholders("plain"), "");
        assert_eq!(placeholders("«redacted:open"), "«redacted:open");
    }

    #[test]
    fn debug_never_shows_the_value() {
        let secret = Secret::new("api_token", "s3cr3t".to_owned());
        assert_eq!(format!("{secret:?}"), "«redacted:api_token»");
        assert_eq!(format!("{secret:#?}"), "«redacted:api_token»");
        assert_eq!(
            format!("{:?}", vec![secret.clone()]),
            "[«redacted:api_token»]"
        );
        assert_eq!(secret.expose(), "s3cr3t");
        assert_eq!(secret.name(), "api_token");
    }
}
