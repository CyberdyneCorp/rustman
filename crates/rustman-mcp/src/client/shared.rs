//! A redactor shared by everything a session shows, which learns OAuth
//! tokens as they are issued and refreshed.

use std::sync::{Arc, PoisonError, RwLock};

use rustman_core::{Redactor, Secret};

#[derive(Clone, Default, Debug)]
pub(crate) struct SharedRedactor(Arc<RwLock<Redactor>>);

impl SharedRedactor {
    pub fn new(redactor: Redactor) -> Self {
        Self(Arc::new(RwLock::new(redactor)))
    }

    /// The redactor as it is now (a cheap clone).
    pub fn get(&self) -> Redactor {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn add(&self, secret: &Secret<String>) {
        self.0
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .add(secret);
    }

    pub fn text(&self, text: &str) -> String {
        self.get().text(text).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_learn_new_secrets() {
        let shared = SharedRedactor::new(Redactor::new());
        let clone = shared.clone();
        shared.add(&Secret::new("token", "canary-t0ken".to_owned()));
        assert!(!clone.text("Bearer canary-t0ken").contains("canary-t0ken"));
    }
}
