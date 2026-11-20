//! Why a bench did not start.

/// A bench refused before any load was generated. Front ends exit with
/// code 2 for [`BenchError::Usage`] and 4 for [`BenchError::Refused`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum BenchError {
    /// Bad flags, a cap exceeded, or a request the bench cannot run.
    #[error("{0}")]
    Usage(String),
    /// The safety policy refused the target or one of the requests.
    #[error("{0}")]
    Refused(String),
}

impl BenchError {
    /// Whether this is a safety refusal (exit code 4) rather than bad input.
    pub fn is_refusal(&self) -> bool {
        matches!(self, Self::Refused(_))
    }
}
