//! `onepassword`: `<vault>/<item>/<field>` through the `op` CLI (`op read`)
//! or, with `connect_host`, a 1Password Connect server whose token comes
//! from `OP_CONNECT_TOKEN` (design D3).

use std::time::Duration;

use rustman_core::secret::Secret;
use rustman_core::secrets::provider::{FetchFuture, LoginFuture, ProviderError, SecretProvider};
use serde_json::Value;

use crate::context::Context;
use crate::process::{self, RunError};
use crate::rest::{Call, Reply, Rest, segment};

/// The environment variable holding the Connect token.
pub const CONNECT_TOKEN_VAR: &str = "OP_CONNECT_TOKEN";
/// The environment variable the 1Password tools read the Connect host from.
pub const CONNECT_HOST_VAR: &str = "OP_CONNECT_HOST";

const NOT_SIGNED_IN: &str = "onepassword: not signed in (run 'op signin')";

pub struct OnePassword {
    ctx: Context,
    backend: Backend,
}

enum Backend {
    Cli {
        op: String,
        account: Option<String>,
    },
    Connect {
        host: String,
        rest: Box<Rest>,
        /// Whether the user chose `host` (user config or `OP_CONNECT_HOST`).
        trusted: bool,
    },
}

impl OnePassword {
    /// The `op` CLI at `op` (a name looked up on `PATH`, or a path from the
    /// user config), signed in to `account` when set.
    pub fn cli(ctx: Context, op: String, account: Option<String>) -> Self {
        Self {
            ctx,
            backend: Backend::Cli { op, account },
        }
    }

    /// The Connect server at `host`.
    pub fn connect(ctx: Context, host: &str, timeout: Duration) -> Self {
        Self {
            ctx,
            backend: Backend::Connect {
                host: host.trim_end_matches('/').to_owned(),
                rest: Box::new(Rest::new(timeout)),
                trusted: true,
            },
        }
    }

    /// Mark the Connect host as chosen by the workspace: `OP_CONNECT_TOKEN`
    /// is then never sent to it.
    #[must_use]
    pub fn untrusted_host(mut self) -> Self {
        if let Backend::Connect { trusted, .. } = &mut self.backend {
            *trusted = false;
        }
        self
    }
}

impl SecretProvider for OnePassword {
    fn fetch<'a>(&'a self, path: &'a str) -> FetchFuture<'a> {
        Box::pin(async move {
            let value = match &self.backend {
                Backend::Cli { op, account } => self.read_cli(op, account.as_deref(), path).await?,
                Backend::Connect { host, rest, .. } => self.read_connect(host, rest, path).await?,
            };
            Ok(Secret::new(path, value))
        })
    }

    /// `op whoami`, or listing the vaults of the Connect server.
    fn check<'a>(&'a self) -> LoginFuture<'a> {
        Box::pin(async move {
            match &self.backend {
                Backend::Cli { op, account } => self.check_cli(op, account.as_deref()).await,
                Backend::Connect { host, rest, .. } => self.check_connect(host, rest).await,
            }
        })
    }
}

impl OnePassword {
    async fn check_cli(&self, op: &str, account: Option<&str>) -> Result<(), ProviderError> {
        let output = self.run_op(op, &["whoami"], account).await?;
        if output.success {
            return Ok(());
        }
        match cli_error(&output.stderr, "") {
            ProviderError::Auth(m) => Err(ProviderError::Auth(m)),
            _ => Err(ProviderError::Unavailable(
                "onepassword: 'op whoami' failed".into(),
            )),
        }
    }

    async fn check_connect(&self, host: &str, rest: &Rest) -> Result<(), ProviderError> {
        let token = self.connect_token()?;
        let call = Call::get(format!("{host}/v1/vaults")).credential("Authorization", &token);
        let reply = rest
            .send(call)
            .await
            .map_err(|e| ProviderError::Unavailable(format!("onepassword: {e}")))?;
        connect_json(&reply, "vaults").map(|_| ())
    }

    fn connect_token(&self) -> Result<Secret<String>, ProviderError> {
        if let Backend::Connect {
            host,
            trusted: false,
            ..
        } = &self.backend
        {
            return Err(ProviderError::Config(format!(
                "onepassword: connect_host {host} is set in rustman.toml; set {CONNECT_HOST_VAR} to it, or 'connect_host' in user config, to send {CONNECT_TOKEN_VAR} there"
            )));
        }
        let token = self.ctx.var(CONNECT_TOKEN_VAR).ok_or_else(|| {
            ProviderError::Auth(format!("onepassword: {CONNECT_TOKEN_VAR} is not set"))
        })?;
        Ok(Secret::new("op_connect_token", format!("Bearer {token}")))
    }

    /// Run `op <args...> [--account <account>]`.
    async fn run_op(
        &self,
        op: &str,
        args: &[&str],
        account: Option<&str>,
    ) -> Result<process::Output, ProviderError> {
        let mut argv = vec![op.to_owned()];
        argv.extend(args.iter().map(|a| (*a).to_owned()));
        if let Some(account) = account {
            argv.extend(["--account".to_owned(), account.to_owned()]);
        }
        process::run(&self.ctx, &argv, None).await.map_err(|e| match e {
            RunError::NotFound => ProviderError::Config(format!(
                "onepassword: '{op}' not found (install the 1Password CLI or set op_path in user config)"
            )),
            RunError::Failed(e) => ProviderError::Unavailable(format!("onepassword: {e}")),
        })
    }

    async fn read_cli(
        &self,
        op: &str,
        account: Option<&str>,
        path: &str,
    ) -> Result<String, ProviderError> {
        // The same `<vault>/<item>/[<section>/]<field>` form as Connect,
        // checked before `op` runs.
        split_path(path)?;
        let output = self
            .run_op(op, &["read", &format!("op://{path}")], account)
            .await?;
        if !output.success {
            return Err(cli_error(&output.stderr, path));
        }
        process::trimmed(&output.stdout)
            .map_err(|_| ProviderError::Unavailable("onepassword: output is not UTF-8".into()))
    }

    async fn read_connect(
        &self,
        host: &str,
        rest: &Rest,
        path: &str,
    ) -> Result<String, ProviderError> {
        let token = self.connect_token()?;
        let (vault, item, field) = split_path(path)?;
        let get = |url: String| async {
            let call = Call::get(url).credential("Authorization", &token);
            let reply = rest
                .send(call)
                .await
                .map_err(|e| ProviderError::Unavailable(format!("onepassword: {e}")))?;
            connect_json(&reply, path)
        };
        let vaults = get(format!("{host}/v1/vaults?filter={}", filter("name", vault))).await?;
        let vault_id = first_id(&vaults, path)?;
        let items_url = format!("{host}/v1/vaults/{}/items", segment(&vault_id));
        let items = get(format!("{items_url}?filter={}", filter("title", item))).await?;
        let item_id = first_id(&items, path)?;
        let full = get(format!("{items_url}/{}", segment(&item_id))).await?;
        field_value(&full, field).ok_or_else(|| ProviderError::FieldNotFound(field.to_owned()))
    }
}

/// The error of a failed `op read`, from its stderr (never echoed whole).
fn cli_error(stderr: &str, path: &str) -> ProviderError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("not currently signed in")
        || lower.contains("not signed in")
        || lower.contains("no accounts configured")
        || lower.contains("session expired")
    {
        return ProviderError::Auth(NOT_SIGNED_IN.to_owned());
    }
    if lower.contains("isn't an item")
        || lower.contains("isn't a vault")
        || lower.contains("isn't a field")
        || lower.contains("not found")
    {
        return ProviderError::NotFound(format!("onepassword: {path} not found"));
    }
    ProviderError::Unavailable(format!("onepassword: 'op read' failed for {path}"))
}

fn split_path(path: &str) -> Result<(&str, &str, &str), ProviderError> {
    let mut parts = path.splitn(3, '/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(v), Some(i), Some(f)) if !v.is_empty() && !i.is_empty() && !f.is_empty() => {
            Ok((v, i, f))
        }
        _ => Err(ProviderError::Config(format!(
            "onepassword: path '{path}' must be <vault>/<item>/<field>"
        ))),
    }
}

/// A Connect `filter` query parameter: `<attr> eq "<value>"`.
fn filter(attribute: &str, value: &str) -> String {
    let expression = format!("{attribute} eq \"{}\"", value.replace('"', "\\\""));
    form_urlencoded::byte_serialize(expression.as_bytes()).collect()
}

fn connect_json(reply: &Reply, path: &str) -> Result<Value, ProviderError> {
    match reply.status {
        200..=299 => reply.json().ok_or_else(|| {
            ProviderError::Unavailable("onepassword: invalid response from Connect".into())
        }),
        401 => Err(ProviderError::Auth(
            "onepassword: authentication failed".into(),
        )),
        403 => Err(ProviderError::Auth(format!(
            "onepassword: permission denied for {path}"
        ))),
        404 => Err(ProviderError::NotFound(format!(
            "onepassword: {path} not found"
        ))),
        status => Err(ProviderError::Unavailable(format!(
            "onepassword: Connect returned {status}"
        ))),
    }
}

fn first_id(list: &Value, path: &str) -> Result<String, ProviderError> {
    list.as_array()
        .and_then(|items| items.first())
        .and_then(|item| item.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ProviderError::NotFound(format!("onepassword: {path} not found")))
}

/// The value of the field whose label or id is `field`.
fn field_value(item: &Value, field: &str) -> Option<String> {
    let fields = item.get("fields")?.as_array()?;
    let matches = |f: &&Value| {
        [f.get("label"), f.get("id")]
            .into_iter()
            .flatten()
            .any(|v| v.as_str() == Some(field))
    };
    let found = fields.iter().find(matches)?;
    Some(found.get("value")?.as_str()?.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_errors_are_classified_without_echoing_stderr() {
        let err = cli_error(
            "[ERROR] You are not currently signed in. Please run `op signin`",
            "a/b/c",
        );
        assert_eq!(err.to_string(), NOT_SIGNED_IN);
        let err = cli_error(
            "[ERROR] \"Stripe\" isn't an item in the \"Eng\" vault",
            "Eng/Stripe/token",
        );
        assert_eq!(err.to_string(), "onepassword: Eng/Stripe/token not found");
        let err = cli_error("boom s3cr3t", "a/b/c");
        assert!(!err.to_string().contains("s3cr3t"));
    }

    #[test]
    fn paths_have_three_parts_and_filters_are_encoded() {
        assert_eq!(
            split_path("Eng/Stripe/token").unwrap(),
            ("Eng", "Stripe", "token")
        );
        assert_eq!(split_path("Eng/Stripe/sec/f").unwrap().2, "sec/f");
        assert!(split_path("Eng/Stripe").is_err());
        assert_eq!(filter("title", "a b"), "title+eq+%22a+b%22");
    }
}
