//! Resolving a `kind: evm-call` request before it is sent, and the fields
//! of a `kind: evm-tx` request before the signing gate checks them
//! (add-evm-toolkit).
//!
//! `{{name}}` is interpolated in `rpc` (the endpoint, with the collection's
//! headers and auth), `to`, `from`, `value`, `block`, the string values of
//! `args` and every string of `state_overrides`. The `[evm] ens_rpc`
//! endpoint and the explorer API keys are resolved too, but only on a best
//! effort basis: a missing variable there matters only when ENS or an
//! explorer is actually needed, and `rustman-evm` reports it then.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::request::{
    ResolvedBody, ResolvedRequest, auth, contain_files, inherit_client_identity, pairs, settings,
    tls_files,
};
use super::resolver::{Interpolator, ResolveError, Resolver, UsedSecret};
use crate::model::{
    AbiSpec, EvmCallRequest, EvmRequest, EvmTxRequest, Request, Scalar, StateOverride,
};
use crate::redact::Redactor;
use crate::secret::Secret;
use crate::workspace::RequestRef;

/// An `evm-call` with every placeholder replaced. It holds secret values:
/// it has no `Debug`, and [`ResolvedEvmCall::redactor`] covers it.
#[derive(Clone)]
pub struct ResolvedEvmCall {
    /// The node: URL, headers and auth, with no body. Its `secrets` hold
    /// every secret used anywhere in the request.
    pub endpoint: ResolvedRequest,
    pub to: String,
    pub from: Option<String>,
    pub value: Option<String>,
    pub args: Vec<Value>,
    pub block: Option<String>,
    pub state_overrides: Option<BTreeMap<String, StateOverride>>,
    /// `[evm] ens_rpc`, when it is set: resolved, or why it did not
    /// resolve (reported when the call needs ENS).
    pub ens_rpc: Option<Result<ResolvedRequest, String>>,
    /// `[evm.explorers.<id>] api_key`, by chain id, for those that resolve.
    pub explorer_keys: BTreeMap<String, Secret<String>>,
}

impl ResolvedEvmCall {
    /// The redactor for everything shown or recorded about this call.
    pub fn redactor(&self) -> Redactor {
        let mut redactor = Redactor::for_request(&self.endpoint);
        for key in self.explorer_keys.values() {
            redactor.add(key);
        }
        redactor
    }
}

/// Resolve the `evm-call` request `request` through `resolver`; `evm` is
/// the `[evm]` table of `rustman.toml`.
pub fn resolve_evm_call(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
    evm: &toml::Table,
) -> Result<ResolvedEvmCall, ResolveError> {
    let call = match request.request() {
        Request::Evm(evm) => match evm.as_ref() {
            EvmRequest::Call(call) => call,
            EvmRequest::Tx(_) => {
                let message = crate::model::EVM_TX_NEEDS_SIGNING.to_owned();
                return Err(ResolveError::Unsupported(message));
            }
        },
        other => {
            let kind = other.kind();
            return Err(ResolveError::Unsupported(format!(
                "request kind '{kind}' is not an evm-call request"
            )));
        }
    };
    let mut i = resolver.interpolator();
    let mut endpoint = endpoint(&mut i, request, call.rpc.get())?;
    let fields = fields(&mut i, call);
    let (mut resolved, secrets) = i.finish(fields)?;
    endpoint.secrets = secrets;
    contain_files(request, &mut endpoint)?;
    resolved.ens_rpc = optional_endpoint(request, resolver, evm, &mut endpoint.secrets);
    if fetches_from_explorer(call, evm) {
        resolved.explorer_keys = explorer_keys(resolver, evm, &mut endpoint.secrets);
    }
    resolved.endpoint = endpoint;
    Ok(resolved)
}

/// An `evm-tx` with every placeholder replaced: what the signing gate
/// checks and the transaction is built from. Nothing here is signed. It
/// holds secret values: it has no `Debug`; `endpoint.secrets` covers it.
#[derive(Clone)]
pub struct ResolvedEvmTx {
    /// The node: URL, headers and auth. Its `secrets` hold every secret
    /// used anywhere in the request.
    pub endpoint: ResolvedRequest,
    pub to: String,
    pub args: Vec<Value>,
    pub value: Option<String>,
    pub gas: Option<String>,
    pub max_fee: Option<String>,
    pub max_priority_fee: Option<String>,
    pub gas_price: Option<String>,
    pub nonce: Option<String>,
    /// The signer id, resolved: the gate refuses a raw key here.
    pub signer: Option<String>,
    /// The transaction fields (`to`, `args`, `value`, gas, fees, `nonce`)
    /// whose value came from a secret, in file order. The gate refuses
    /// them: a confirmation must show what is signed.
    pub secret_fields: Vec<&'static str>,
}

/// Resolve the `evm-tx` request `request` through `resolver`.
pub fn resolve_evm_tx(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
) -> Result<ResolvedEvmTx, ResolveError> {
    let Request::Evm(evm) = request.request() else {
        return Err(not_a_tx(request));
    };
    let EvmRequest::Tx(tx) = evm.as_ref() else {
        return Err(not_a_tx(request));
    };
    let mut i = resolver.interpolator();
    let mut endpoint = endpoint(&mut i, request, tx.rpc.get())?;
    let fields = tx_fields(&mut i, tx);
    let (mut resolved, secrets) = i.finish(fields)?;
    endpoint.secrets = secrets;
    contain_files(request, &mut endpoint)?;
    resolved.endpoint = endpoint;
    Ok(resolved)
}

/// Resolve `text` (such as a signer's keystore `password`, usually a
/// `{{secret:...}}` reference). The value is returned as a secret named
/// `name`, so it is redacted wherever it could appear, with the secrets
/// used to build it.
pub fn resolve_evm_secret(
    resolver: &Resolver<'_>,
    name: &str,
    text: &str,
) -> Result<(Secret<String>, Vec<UsedSecret>), ResolveError> {
    let mut i = resolver.interpolator();
    let value = i.text(text).text;
    let ((), mut used) = i.finish(())?;
    let secret = Secret::new(name, value);
    used.push(secret.clone());
    Ok((secret, used))
}

fn not_a_tx(request: &RequestRef<'_>) -> ResolveError {
    let kind = request.request().kind();
    ResolveError::Unsupported(format!("request kind '{kind}' is not an evm-tx request"))
}

fn tx_fields(i: &mut Interpolator<'_, '_>, tx: &EvmTxRequest) -> ResolvedEvmTx {
    let mut secret_fields = Vec::new();
    let mut field = |name: &'static str, text: &str, secret_fields: &mut Vec<&'static str>| {
        let resolved = i.text(text);
        if !resolved.secret_spans.is_empty() {
            secret_fields.push(name);
        }
        resolved.text
    };
    let to = tx
        .to
        .as_ref()
        .map(|t| field("to", t.get(), &mut secret_fields))
        .unwrap_or_default();
    let mut from_secret = false;
    let args = tx
        .args
        .iter()
        .flatten()
        .map(|a| string_values(i, a, &mut from_secret))
        .collect();
    if from_secret {
        secret_fields.push("args");
    }
    let mut scalar = |name: &'static str, s: &Option<Scalar>| {
        s.as_ref().map(|s| {
            let resolved = i.text(s.as_str());
            if !resolved.secret_spans.is_empty() {
                secret_fields.push(name);
            }
            resolved.text
        })
    };
    let value = scalar("value", &tx.value);
    let gas = scalar("gas", &tx.gas);
    let max_fee = scalar("max_fee", &tx.max_fee);
    let max_priority_fee = scalar("max_priority_fee", &tx.max_priority_fee);
    let gas_price = scalar("gas_price", &tx.gas_price);
    let nonce = scalar("nonce", &tx.nonce);
    let signer = tx.signer.as_deref().map(|s| i.text(s).text);
    ResolvedEvmTx {
        endpoint: empty_endpoint(),
        to,
        args,
        value,
        gas,
        max_fee,
        max_priority_fee,
        gas_price,
        nonce,
        signer,
        secret_fields,
    }
}

/// Resolve `url` (a URL or a `{{variable}}` holding one) as an EVM node
/// endpoint in the context of `request`: its headers and auth apply. The
/// `rustman evm` commands that take `--rpc` use it.
pub fn resolve_evm_endpoint(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
    url: &str,
) -> Result<ResolvedRequest, ResolveError> {
    let mut i = resolver.interpolator();
    let endpoint = endpoint(&mut i, request, url)?;
    let (mut endpoint, secrets) = i.finish(endpoint)?;
    endpoint.secrets = secrets;
    contain_files(request, &mut endpoint)?;
    Ok(endpoint)
}

/// A POST to `url` with the request's inherited headers and auth.
fn endpoint(
    i: &mut Interpolator<'_, '_>,
    request: &RequestRef<'_>,
    url: &str,
) -> Result<ResolvedRequest, ResolveError> {
    let mut settings = settings(i, None);
    let mut tls = tls_files(&settings);
    inherit_client_identity(i, request, &mut settings, &mut tls)?;
    Ok(ResolvedRequest {
        method: "POST".to_owned(),
        url: i.text(url),
        query: Vec::new(),
        headers: pairs(i, &request.effective_headers()),
        body: ResolvedBody::None,
        auth: request.effective_auth().map(|a| auth(i, a)),
        settings,
        tls,
        secrets: Vec::new(),
    })
}

fn fields(i: &mut Interpolator<'_, '_>, call: &EvmCallRequest) -> ResolvedEvmCall {
    let mut text = |s: &str| i.text(s).text;
    let to = call.to.as_ref().map(|t| text(t.get())).unwrap_or_default();
    let from = call.from.as_deref().map(&mut text);
    let value = call.value.as_ref().map(|v| text(v.as_str()));
    let block = call.block.as_ref().map(|b| text(b.as_str()));
    let args = call
        .args
        .iter()
        .flatten()
        .map(|a| string_values(i, a, &mut false))
        .collect();
    let state_overrides = call.state_overrides.as_ref().map(|o| overrides(i, o));
    ResolvedEvmCall {
        endpoint: empty_endpoint(),
        to,
        from,
        value,
        args,
        block,
        state_overrides,
        ens_rpc: None,
        explorer_keys: BTreeMap::new(),
    }
}

fn empty_endpoint() -> ResolvedRequest {
    ResolvedRequest {
        method: "POST".to_owned(),
        url: Default::default(),
        query: Vec::new(),
        headers: Vec::new(),
        body: ResolvedBody::None,
        auth: None,
        settings: Default::default(),
        tls: Default::default(),
        secrets: Vec::new(),
    }
}

fn overrides(
    i: &mut Interpolator<'_, '_>,
    overrides: &BTreeMap<String, StateOverride>,
) -> BTreeMap<String, StateOverride> {
    let scalar = |s: &Option<Scalar>, i: &mut Interpolator<'_, '_>| {
        s.as_ref().map(|s| Scalar(i.text(s.as_str()).text))
    };
    overrides
        .iter()
        .map(|(address, state)| {
            let resolved = StateOverride {
                balance: scalar(&state.balance, i),
                nonce: scalar(&state.nonce, i),
                code: state.code.as_deref().map(|c| i.text(c).text),
                storage: state.storage.as_ref().map(|slots| {
                    slots
                        .iter()
                        .map(|(k, v)| (i.text(k).text, i.text(v).text))
                        .collect()
                }),
            };
            (i.text(address).text, resolved)
        })
        .collect()
}

/// `value` with every string interpolated; keys and other scalars are kept.
/// `value` with every string interpolated; `from_secret` is set when a
/// secret went into one.
fn string_values(i: &mut Interpolator<'_, '_>, value: &Value, from_secret: &mut bool) -> Value {
    match value {
        Value::String(text) => {
            let resolved = i.text(text);
            *from_secret |= !resolved.secret_spans.is_empty();
            Value::String(resolved.text)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| string_values(i, v, from_secret))
                .collect(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), string_values(i, v, from_secret)))
                .collect::<Map<String, Value>>(),
        ),
        other => other.clone(),
    }
}

/// `[evm] ens_rpc`, when set: resolved, or why it did not resolve.
fn optional_endpoint(
    request: &RequestRef<'_>,
    resolver: &Resolver<'_>,
    evm: &toml::Table,
    secrets: &mut Vec<UsedSecret>,
) -> Option<Result<ResolvedRequest, String>> {
    let url = evm.get("ens_rpc")?.as_str()?;
    let mut i = resolver.interpolator();
    let resolved = endpoint(&mut i, request, url).and_then(|e| i.finish(e));
    Some(match resolved {
        Ok((mut endpoint, used)) => {
            secrets.extend(used.iter().cloned());
            endpoint.secrets = used;
            Ok(endpoint)
        }
        Err(error) => Err(format!("[evm] ens_rpc: {error}")),
    })
}

/// The explorer API keys that resolve, by chain id.
/// Whether the call may fetch its ABI from an explorer: `abi: {explorer:
/// true}` with `[evm] allow_explorer_fetch = true`. Only then are the
/// explorer API keys resolved, so no secret provider is asked for a key
/// that is not used.
fn fetches_from_explorer(call: &EvmCallRequest, evm: &toml::Table) -> bool {
    let explorer = matches!(call.abi.as_ref().map(|a| a.get()), Some(AbiSpec::Explorer));
    let allowed = evm
        .get("allow_explorer_fetch")
        .and_then(toml::Value::as_bool);
    explorer && allowed == Some(true)
}

fn explorer_keys(
    resolver: &Resolver<'_>,
    evm: &toml::Table,
    secrets: &mut Vec<UsedSecret>,
) -> BTreeMap<String, Secret<String>> {
    let Some(explorers) = evm.get("explorers").and_then(toml::Value::as_table) else {
        return BTreeMap::new();
    };
    let mut keys = BTreeMap::new();
    for (chain, explorer) in explorers {
        let Some(raw) = explorer.get("api_key").and_then(toml::Value::as_str) else {
            continue;
        };
        let mut i = resolver.interpolator();
        let text = i.text(raw).text;
        if let Ok(((), used)) = i.finish(()) {
            secrets.extend(used);
            let key = Secret::new(format!("{chain}_api_key"), text);
            secrets.push(key.clone());
            keys.insert(chain.clone(), key);
        }
    }
    keys
}
