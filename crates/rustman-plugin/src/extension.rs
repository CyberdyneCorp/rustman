//! The extension points a plugin can implement, one WIT world each
//! (design D2).

use std::fmt;

/// An extension point, as a manifest names it in `extension_points`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExtensionPoint {
    Auth,
    Assertion,
    Generator,
    Transformer,
    Importer,
    Kind,
    Report,
}

impl ExtensionPoint {
    pub const ALL: [Self; 7] = [
        Self::Auth,
        Self::Assertion,
        Self::Generator,
        Self::Transformer,
        Self::Importer,
        Self::Kind,
        Self::Report,
    ];

    /// The manifest name, which is also the WIT world name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auth => "auth",
            Self::Assertion => "assertion",
            Self::Generator => "generator",
            Self::Transformer => "transformer",
            Self::Importer => "importer",
            Self::Kind => "kind",
            Self::Report => "report",
        }
    }

    /// The interface the world exports.
    pub fn interface(self) -> &'static str {
        match self {
            Self::Auth => "auth-scheme",
            Self::Assertion => "assertion-fn",
            Self::Generator => "value-generator",
            Self::Transformer => "body-transformer",
            Self::Importer => "format-importer",
            Self::Kind => "request-kind",
            Self::Report => "report-renderer",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|point| point.as_str() == name)
    }
}

impl fmt::Display for ExtensionPoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_point_matches_a_world_of_the_wit_package() {
        for point in ExtensionPoint::ALL {
            assert_eq!(ExtensionPoint::parse(point.as_str()), Some(point));
            let world = (point.as_str(), point.interface());
            assert!(rustman_plugin_sdk::WORLDS.contains(&world), "{world:?}");
        }
        assert_eq!(ExtensionPoint::parse("telemetry"), None);
    }
}
