//! The `Prefer` request header, compatible with Prism (spec: Prefer
//! header): `code=<status>`, `example=<name>` and `dynamic=true`.

/// What a client asked for with `Prefer`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prefer {
    /// The status code wanted, as written (`404`).
    pub code: Option<String>,
    /// A named example of the chosen response.
    pub example: Option<String>,
    /// Force schema synthesis.
    pub dynamic: bool,
}

impl Prefer {
    /// The preferences in the `Prefer` header values; preferences are
    /// separated by `,` or `;`, unknown ones are ignored.
    pub fn parse<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut prefer = Self::default();
        let tokens = values.into_iter().flat_map(|v| v.split([',', ';']));
        for token in tokens {
            let Some((key, value)) = token.split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"').to_owned();
            match key.trim().to_ascii_lowercase().as_str() {
                "code" => prefer.code = Some(value),
                "example" => prefer.example = Some(value),
                "dynamic" => prefer.dynamic = value.eq_ignore_ascii_case("true"),
                _ => {}
            }
        }
        prefer
    }

    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_example_and_dynamic() {
        let prefer = Prefer::parse(["code=404, example=notFound", "dynamic=true"]);
        assert_eq!(prefer.code.as_deref(), Some("404"));
        assert_eq!(prefer.example.as_deref(), Some("notFound"));
        assert!(prefer.dynamic);
        let quoted = Prefer::parse(["respond-async; code=\"201\"; dynamic=false"]);
        assert_eq!(quoted.code.as_deref(), Some("201"));
        assert!(!quoted.dynamic);
        assert!(Prefer::parse(["return=minimal"]).is_empty());
    }
}
