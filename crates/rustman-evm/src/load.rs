//! The load-time checks of `evm-call` and `evm-tx` files that need the
//! ABI: the function exists and is not ambiguous, the argument count fits,
//! and literal quantities and block selectors parse.
//!
//! Only local sources are read (built-in names and workspace files). An
//! explorer ABI is checked when it is fetched. Nothing here calls a node
//! or touches a signer.

use std::path::Path;

use alloy_json_abi::Function;
use rustman_core::model::{AbiSpec, EvmRequest, Scalar};
use rustman_core::{LoadError, Position, Workspace};

use crate::abi::{AbiCache, LoadedAbi, resolve_function};
use crate::quantity::{Denomination, Quantity};

/// What the checks found for one request.
#[derive(Clone, Debug)]
pub struct CheckedRequest {
    /// `None` for `abi: {explorer: true}`.
    pub abi: Option<LoadedAbi>,
    pub function: Option<Function>,
}

/// The named block tags `block:` accepts.
pub const BLOCK_TAGS: [&str; 5] = ["latest", "earliest", "pending", "safe", "finalized"];

/// Check the request file `file` (relative to the workspace `root`).
pub fn check(
    root: &Path,
    file: &Path,
    request: &EvmRequest,
    cache: &AbiCache,
) -> Result<CheckedRequest, LoadError> {
    let common = request.common();
    let fail =
        |position: Option<Position>, message: String| LoadError::new(file, position, message);
    check_literals(request).map_err(|m| fail(None, m))?;
    let Some(abi_spec) = common.abi else {
        return Ok(CheckedRequest {
            abi: None,
            function: None,
        });
    };
    let source = match abi_spec.get() {
        AbiSpec::Explorer => {
            return Ok(CheckedRequest {
                abi: None,
                function: None,
            });
        }
        AbiSpec::Source(source) => source,
    };
    let loaded = cache
        .load(root, file, source)
        .map_err(|e| fail(abi_spec.position(), e.to_string()))?;
    let Some(spec) = common.function else {
        return Ok(CheckedRequest {
            abi: Some(loaded),
            function: None,
        });
    };
    let function = resolve_function(&loaded.abi, spec)
        .map_err(|e| fail(spec.position(), e.to_string()))?
        .clone();
    if function.inputs.len() != common.args.len() {
        let message = format!(
            "function '{}' takes {} argument(s), got {}",
            function.signature(),
            function.inputs.len(),
            common.args.len()
        );
        return Err(fail(spec.position(), message));
    }
    Ok(CheckedRequest {
        abi: Some(loaded),
        function: Some(function),
    })
}

/// Check every `evm-call` and `evm-tx` request of `workspace`.
/// The load-time check front ends register with
/// [`rustman_core::workspace::register_workspace_check`]: a workspace with
/// a bad `evm-call` or `evm-tx` file (an overloaded function named without
/// its signature, an unknown function, a wrong argument count) fails to
/// load, before anything is sent.
pub fn workspace_check(workspace: &Workspace) -> Vec<LoadError> {
    check_workspace(workspace, &AbiCache::new())
}

pub fn check_workspace(workspace: &Workspace, cache: &AbiCache) -> Vec<LoadError> {
    workspace
        .requests()
        .into_iter()
        .filter_map(|r| match r.request() {
            rustman_core::model::Request::Evm(evm) => {
                check(&workspace.root, &r.entry.path, evm, cache).err()
            }
            _ => None,
        })
        .collect()
}

fn templated(text: &str) -> bool {
    text.contains("{{")
}

/// `value` (and the fee fields of an `evm-tx`) and `block`, unless they
/// come from a variable.
fn check_literals(request: &EvmRequest) -> Result<(), String> {
    let quantities: Vec<(&str, Option<&Scalar>)> = match request {
        EvmRequest::Call(call) => vec![("value", call.value.as_ref())],
        EvmRequest::Tx(tx) => vec![
            ("value", tx.value.as_ref()),
            ("gas", tx.gas.as_ref()),
            ("max_fee", tx.max_fee.as_ref()),
            ("max_priority_fee", tx.max_priority_fee.as_ref()),
            ("gas_price", tx.gas_price.as_ref()),
            ("nonce", tx.nonce.as_ref()),
        ],
    };
    let native = Denomination::default();
    for (name, scalar) in quantities {
        let Some(text) = scalar.map(Scalar::as_str).filter(|t| !templated(t)) else {
            continue;
        };
        Quantity::parse(text, &native).map_err(|e| format!("{name}: {e}"))?;
    }
    if let EvmRequest::Call(call) = request {
        if let Some(block) = call.block.as_ref().map(Scalar::as_str) {
            check_block(block)?;
        }
        for (address, state) in call.state_overrides.iter().flatten() {
            check_override(address, state)?;
        }
    }
    Ok(())
}

/// A block tag, a decimal or hex number, or a 32-byte block hash.
pub fn check_block(block: &str) -> Result<(), String> {
    let ok = templated(block)
        || BLOCK_TAGS.contains(&block)
        || block.bytes().all(|b| b.is_ascii_digit()) && !block.is_empty()
        || is_hex(block, None);
    if ok {
        Ok(())
    } else {
        Err(format!(
            "block: '{block}' is not a block tag ({}), number or hash",
            BLOCK_TAGS.join(", ")
        ))
    }
}

fn is_hex(text: &str, bytes: Option<usize>) -> bool {
    let Some(digits) = text.strip_prefix("0x") else {
        return false;
    };
    let length_ok = bytes.is_none_or(|n| digits.len() == n * 2);
    !digits.is_empty() && length_ok && digits.bytes().all(|b| b.is_ascii_hexdigit())
}

fn check_override(address: &str, state: &rustman_core::model::StateOverride) -> Result<(), String> {
    if !templated(address) && !is_hex(address, Some(20)) {
        return Err(format!(
            "state_overrides: '{address}' is not a 20-byte hex address"
        ));
    }
    let native = Denomination::default();
    for (name, scalar) in [("balance", &state.balance), ("nonce", &state.nonce)] {
        if let Some(text) = scalar
            .as_ref()
            .map(Scalar::as_str)
            .filter(|t| !templated(t))
        {
            Quantity::parse(text, &native)
                .map_err(|e| format!("state_overrides.{address}.{name}: {e}"))?;
        }
    }
    let code = state
        .code
        .as_deref()
        .filter(|c| !templated(c) && *c != "0x");
    if code.is_some_and(|c| !is_hex(c, None)) {
        return Err(format!("state_overrides.{address}.code: not hex"));
    }
    for (slot, value) in state.storage.iter().flatten() {
        let hex_or_templated = |text: &str| templated(text) || is_hex(text, None);
        if !hex_or_templated(slot) || !hex_or_templated(value) {
            return Err(format!(
                "state_overrides.{address}.storage: slots and values must be hex"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_selectors() {
        for ok in ["latest", "finalized", "19000000", "0x121eac0", "{{block}}"] {
            assert!(check_block(ok).is_ok(), "{ok}");
        }
        let err = check_block("newest").unwrap_err();
        assert_eq!(
            err,
            "block: 'newest' is not a block tag (latest, earliest, pending, safe, finalized), number or hash"
        );
        assert!(check_block("").is_err() && check_block("0x").is_err());
    }

    #[test]
    fn templated_storage_overrides_load() {
        let address = "0x00000000000000000000000000000000000000aa";
        let with_storage = |slot: &str, value: &str| rustman_core::model::StateOverride {
            balance: None,
            nonce: None,
            code: None,
            storage: Some([(slot.to_owned(), value.to_owned())].into()),
        };
        assert!(check_override(address, &with_storage("{{slot}}", "{{value}}")).is_ok());
        assert!(check_override(address, &with_storage("0x0", "{{value}}")).is_ok());
        let err = check_override(address, &with_storage("slot0", "0x1")).unwrap_err();
        assert!(
            err.ends_with("storage: slots and values must be hex"),
            "{err}"
        );
    }
}
