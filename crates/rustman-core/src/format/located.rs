use std::fmt;
use std::ops::Deref;

use serde::{Deserialize, Deserializer};

use super::position_of;
use crate::error::Position;

/// A value plus the position it was read from.
///
/// Equality ignores the position, so a value built in code equals the same
/// value loaded from a file.
#[derive(Clone)]
pub struct Located<T> {
    value: T,
    position: Option<Position>,
}

impl<T> Located<T> {
    /// A value with no source position (built in code rather than loaded).
    pub fn new(value: T) -> Self {
        Self {
            value,
            position: None,
        }
    }

    pub fn position(&self) -> Option<Position> {
        self.position
    }

    pub fn get(&self) -> &T {
        &self.value
    }

    pub fn into_inner(self) -> T {
        self.value
    }
}

impl<T> Deref for Located<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.value
    }
}

impl<T: PartialEq> PartialEq for Located<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq> Eq for Located<T> {}

impl<T: fmt::Debug> fmt::Debug for Located<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.value.fmt(f)
    }
}

impl<T> From<T> for Located<T> {
    fn from(value: T) -> Self {
        Self::new(value)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Located<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let spanned = serde_saphyr::Spanned::<T>::deserialize(deserializer)?;
        Ok(Self {
            position: position_of(spanned.referenced),
            value: spanned.value,
        })
    }
}
