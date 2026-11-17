//! Method gating: which methods a policy lets through.

/// How much harm a method can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MethodClass {
    /// GET, HEAD and OPTIONS.
    ReadOnly,
    /// POST, PUT and PATCH.
    Mutation,
    /// DELETE, and every other method (TRACE, CONNECT, custom methods such as
    /// PURGE, and lowercase spellings, since methods are case-sensitive): what
    /// a method we don't know does can't be assumed safe.
    Destructive,
}

impl MethodClass {
    pub fn of(method: &str) -> Self {
        match method {
            "GET" | "HEAD" | "OPTIONS" => Self::ReadOnly,
            "POST" | "PUT" | "PATCH" => Self::Mutation,
            _ => Self::Destructive,
        }
    }
}

/// The methods a policy allows, from the `--allow-mutations` and
/// `--allow-destructive` flags. Each level allows everything below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Permission {
    /// The default.
    #[default]
    ReadOnly,
    /// `--allow-mutations`.
    Mutations,
    /// `--allow-destructive`, which implies `--allow-mutations`.
    Destructive,
}

impl Permission {
    pub fn from_flags(allow_mutations: bool, allow_destructive: bool) -> Self {
        if allow_destructive {
            Self::Destructive
        } else if allow_mutations {
            Self::Mutations
        } else {
            Self::ReadOnly
        }
    }

    pub fn allows(self, class: MethodClass) -> bool {
        match class {
            MethodClass::ReadOnly => true,
            MethodClass::Mutation => self >= Self::Mutations,
            MethodClass::Destructive => self == Self::Destructive,
        }
    }
}
