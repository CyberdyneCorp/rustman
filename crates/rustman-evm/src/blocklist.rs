//! The send-method blocklist (evm-toolkit "Send methods blocked outside the
//! signing gate", design D3) and the `@evm-read` method preset ("EVM
//! read-method preset").
//!
//! [`install`] registers a `rustman-jsonrpc` method guard that refuses
//! every method able to sign or send a transaction, for every `jsonrpc`
//! and `evm-call` request and every agent call, and for every `kind: http`
//! request whose body is a JSON-RPC call or batch, before anything is sent.
//! Signing is not available in this build, so the local-chain opt-in
//! (`[evm] allow_raw_send_local`) is not honored yet: the guard always
//! refuses.

use std::sync::{Arc, Once};

use rustman_jsonrpc::guard::{MethodGuard, register_guard, register_preset};
use serde_json::Value;

/// The refusal message (exit code 4).
pub const SEND_REFUSED: &str = "signing and sending go through 'evm-tx' (signing gate)";

/// Namespaces whose every method may sign, send, unlock or import keys:
/// `personal_*` (Geth), `wallet_*` (EIP-5792 and wallet RPCs), `account_*`
/// (Clef) and `signer_*` (Parity).
pub const SEND_NAMESPACES: [&str; 4] = ["personal_", "wallet_", "account_", "signer_"];

/// How the name of a signing or sending method starts, after its
/// namespace: `eth_send*` (including `eth_sendTransactionSync`,
/// `eth_sendRawTransactionConditional`, `eth_sendBundle`,
/// `eth_sendPrivateTransaction`), `eth_sign*`, `flashbots_sendBundle`,
/// `mev_sendBundle`, `eth_submitTransaction`, `parity_postTransaction`.
pub const SEND_VERBS: [&str; 4] = ["send", "sign", "submittransaction", "posttransaction"];

/// The name of the read-only preset, as written in
/// `[jsonrpc] agent_read_only_methods`.
pub const EVM_READ_PRESET: &str = "@evm-read";

/// The method globs `@evm-read` expands to: reads only.
pub const EVM_READ_METHODS: [&str; 14] = [
    "eth_chainId",
    "eth_blockNumber",
    "eth_call",
    "eth_estimateGas",
    "eth_getBalance",
    "eth_getCode",
    "eth_getStorageAt",
    "eth_getBlockBy*",
    "eth_getTransaction*",
    "eth_getLogs",
    "eth_feeHistory",
    "eth_gasPrice",
    "net_version",
    "web3_clientVersion",
];

/// Whether `method` may sign or send: a method of a [`SEND_NAMESPACES`]
/// namespace, or a namespaced method (`ns_name`, the Ethereum style) whose
/// name after its namespace starts with one of [`SEND_VERBS`]. Methods
/// without a namespace (Bitcoin's `sendtoaddress`) belong to other chains'
/// JSON-RPC and are left alone. Compared without case, as some nodes accept any case.
/// Matching by family, not by exact name, keeps new sending methods
/// (`eth_sendRawTransactionSync`, ...) out.
pub fn is_send_method(method: &str) -> bool {
    let lower = method.trim().to_ascii_lowercase();
    if SEND_NAMESPACES.iter().any(|ns| lower.starts_with(ns)) {
        return true;
    }
    let Some((_, name)) = lower.split_once('_') else {
        return false;
    };
    SEND_VERBS.iter().any(|verb| name.starts_with(verb))
}

/// The guard [`install`] registers.
#[derive(Clone, Copy, Debug, Default)]
pub struct SendBlocklist;

impl MethodGuard for SendBlocklist {
    fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
        is_send_method(method).then(|| SEND_REFUSED.to_owned())
    }
}

/// The method guards' refusal of a JSON-RPC call found in a plain http
/// request body.
fn guarded_body_method(method: &str, params: Option<&Value>) -> Option<String> {
    rustman_jsonrpc::guard::check_method(method, params).map(|refusal| refusal.message)
}

/// Register the send-method guard and the `@evm-read` preset, once per
/// process, and have plain http requests whose body is JSON-RPC checked
/// by the guards too. Front ends call it at startup, before any request is
/// sent.
pub fn install() {
    static INSTALL: Once = Once::new();
    INSTALL.call_once(|| {
        register_guard(Arc::new(SendBlocklist));
        register_preset(EVM_READ_PRESET, &EVM_READ_METHODS);
        rustman_http::runner::set_jsonrpc_body_check(guarded_body_method);
    });
}

#[cfg(test)]
mod tests {
    use rustman_jsonrpc::guard::{check_method, preset};

    use super::*;

    #[test]
    fn send_and_sign_methods_are_refused() {
        let guard = SendBlocklist;
        for method in [
            "eth_sendRawTransaction",
            "eth_sendTransaction",
            "eth_sign",
            "personal_sign",
            "eth_signTransaction",
            "eth_signTypedData",
            "eth_signTypedData_v4",
            "ETH_SENDRAWTRANSACTION",
            "personal_sendTransaction",
        ] {
            assert_eq!(
                guard.check(method, None).as_deref(),
                Some(SEND_REFUSED),
                "{method}"
            );
        }
        for method in [
            "eth_call",
            "eth_getBalance",
            "eth_estimateGas",
            "eth_syncing",
            "eth_subscribe",
            "eth_simulateV1",
            "net_version",
            "web3_clientVersion",
            "debug_traceCall",
            "anvil_setBalance",
        ] {
            assert_eq!(guard.check(method, None), None, "{method}");
        }
    }

    #[test]
    fn every_family_of_sending_methods_is_refused() {
        // Regression: only seven exact names were refused, so
        // eth_sendTransactionSync made Anvil sign and mine a transaction.
        let families: [(&str, &[&str]); 6] = [
            (
                "sync sends (EIP-7966)",
                &["eth_sendTransactionSync", "eth_sendRawTransactionSync"],
            ),
            (
                "conditional, bundle and private sends",
                &[
                    "eth_sendRawTransactionConditional",
                    "eth_sendBundle",
                    "eth_sendPrivateTransaction",
                    "eth_sendPrivateRawTransaction",
                    "flashbots_sendBundle",
                    "mev_sendBundle",
                    "eth_sendUnsignedTransaction",
                ],
            ),
            (
                "wallet",
                &[
                    "wallet_sendCalls",
                    "wallet_sendTransaction",
                    "wallet_grantPermissions",
                ],
            ),
            (
                "personal",
                &[
                    "personal_unlockAccount",
                    "personal_importRawKey",
                    "personal_newAccount",
                ],
            ),
            (
                "clef and parity signers",
                &[
                    "account_signTransaction",
                    "signer_confirmRequest",
                    "parity_postTransaction",
                ],
            ),
            ("submit", &["eth_submitTransaction"]),
        ];
        for (family, methods) in families {
            for method in methods {
                assert!(is_send_method(method), "{family}: {method}");
                let upper = method.to_ascii_uppercase();
                assert!(is_send_method(&upper), "{family}: {upper}");
            }
        }
    }

    #[test]
    fn install_registers_the_guard_and_the_preset_once() {
        install();
        install();
        let refusal = check_method("eth_sendRawTransaction", None).unwrap();
        assert_eq!(refusal.message, SEND_REFUSED);
        assert!(check_method("eth_getBalance", None).is_none());
        let globs = preset("evm-read").unwrap();
        assert!(globs.iter().any(|g| g == "eth_getBalance"));
        assert!(!globs.iter().any(|g| is_send_method(g)));
    }
}
