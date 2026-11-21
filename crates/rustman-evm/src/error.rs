//! The errors of the EVM toolkit, each with its ROADMAP exit code.

/// What went wrong, and so which exit code a front end uses.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EvmError {
    /// A bad file, setting, argument or ABI (exit code 2).
    #[error("{0}")]
    Config(String),
    /// The node could not be reached or answered with an error (exit 3).
    #[error("{0}")]
    Rpc(String),
    /// Refused for safety, such as a chain mismatch (exit code 4).
    #[error("{0}")]
    Refused(String),
    /// The thing asked for does not exist, such as a transaction (exit 1).
    #[error("{0}")]
    NotFound(String),
}

impl EvmError {
    pub fn config(message: impl Into<String>) -> Self {
        Self::Config(message.into())
    }

    /// The exit code of the ROADMAP table.
    pub fn exit_code(&self) -> u8 {
        match self {
            Self::NotFound(_) => 1,
            Self::Config(_) => 2,
            Self::Rpc(_) => 3,
            Self::Refused(_) => 4,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_follow_the_roadmap() {
        let codes: Vec<u8> = [
            EvmError::NotFound("x".into()),
            EvmError::config("x"),
            EvmError::Rpc("x".into()),
            EvmError::Refused("x".into()),
        ]
        .iter()
        .map(EvmError::exit_code)
        .collect();
        assert_eq!(codes, [1, 2, 3, 4]);
        assert_eq!(EvmError::config("bad").to_string(), "bad");
    }
}
