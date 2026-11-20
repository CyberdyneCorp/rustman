//! Secrets as handles (design D6): a plugin can look a secret up by name,
//! but its value is revealed only with `secrets:read:<name>`.

/// The host side of a `secret-ref` resource: the name it stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecretHandle {
    pub name: String,
}
