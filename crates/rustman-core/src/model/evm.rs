//! The `kind: evm-call` and `kind: evm-tx` request files (evm-toolkit
//! "evm-call request files", evm-signing "evm-tx request files").
//!
//! ABI handling, encoding and sending belong to the `rustman-evm` crate;
//! this module only holds the file models and their schema checks. Loading
//! never reads an ABI, calls a node or touches a signer.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::assertion::Assertion;
use super::capture::Capture;
use super::common::{Checked, Variables};
use super::docs::DocsMeta;
use crate::error::LoadError;
use crate::format::{Located, from_yaml};

/// The kind name of a read-only contract call.
pub const EVM_CALL_KIND: &str = "evm-call";

/// The kind name of a signed transaction.
pub const EVM_TX_KIND: &str = "evm-tx";

/// The load error of an `evm-tx` file without `to`.
pub const DEPLOYMENT_OUT_OF_SCOPE: &str =
    "contract deployment is out of scope; use Foundry or Hardhat";

/// Why an `evm-tx` request cannot run: signing ships in a later release.
pub const SIGNING_UNAVAILABLE: &str =
    "evm-tx: signing is not available yet; nothing was signed or sent";

/// A loaded `evm-call` or `evm-tx` file.
#[derive(Clone, Debug, PartialEq)]
pub enum EvmRequest {
    Call(EvmCallRequest),
    Tx(EvmTxRequest),
}

/// A plain value written as a string or an unsigned integer: quantities
/// (`1.5 ether`, `0x1`, `1000`) and block numbers or tags. Kept as text;
/// `rustman-evm` parses it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Scalar(pub String);

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl de::Visitor<'_> for Visitor {
            type Value = Scalar;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or an unsigned integer")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Scalar, E> {
                Ok(Scalar(v.to_owned()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Scalar, E> {
                Ok(Scalar(v.to_string()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Scalar, E> {
                u64::try_from(v)
                    .map(|v| Scalar(v.to_string()))
                    .map_err(|_| E::invalid_value(de::Unexpected::Signed(v), &self))
            }
        }
        deserializer.deserialize_any(Visitor)
    }
}

impl Scalar {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// `abi:` a built-in name (`erc20`, `erc721`, `erc1155`) or a path to a
/// JSON ABI or a Foundry/Hardhat artifact, or `{explorer: true}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AbiSpec {
    Source(String),
    Explorer,
}

impl<'de> Deserialize<'de> for AbiSpec {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Explorer {
            explorer: bool,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Source(String),
            Explorer(Explorer),
        }
        let message = "abi must be a built-in name, a path, or {explorer: true}";
        match Raw::deserialize(deserializer).map_err(|_| de::Error::custom(message))? {
            Raw::Source(source) if source.trim().is_empty() => Err(de::Error::custom(message)),
            Raw::Source(source) => Ok(Self::Source(source)),
            Raw::Explorer(Explorer { explorer: true }) => Ok(Self::Explorer),
            Raw::Explorer(_) => Err(de::Error::custom(message)),
        }
    }
}

/// One address of `state_overrides`: what `eth_call` sees instead of the
/// chain's state.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateOverride {
    pub balance: Option<Scalar>,
    pub nonce: Option<Scalar>,
    /// Runtime bytecode, `0x`-prefixed hex.
    pub code: Option<String>,
    /// Storage slots replaced one by one (`stateDiff`).
    pub storage: Option<BTreeMap<String, String>>,
}

/// `type:` of an `evm-tx` file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TxType {
    #[default]
    Eip1559,
    Legacy,
}

/// A `kind: evm-call` request file: a read-only `eth_call`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvmCallRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub docs: Option<DocsMeta>,
    /// The node's URL or a `{{variable}}`.
    pub rpc: Located<String>,
    /// A contract address or an ENS name.
    pub to: Option<Located<String>>,
    pub abi: Option<Located<AbiSpec>>,
    /// A function name, or a full signature for overloaded functions.
    pub function: Option<Located<String>>,
    pub args: Option<Vec<Value>>,
    pub from: Option<String>,
    pub value: Option<Scalar>,
    /// A block tag, number or hash (default `latest`).
    pub block: Option<Scalar>,
    pub state_overrides: Option<BTreeMap<String, StateOverride>>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    /// The file text, written back unchanged on save.
    #[serde(skip)]
    pub source: String,
}

/// A `kind: evm-tx` request file. Loading it never signs or sends.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvmTxRequest {
    pub version: u64,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub tags: Option<Vec<String>>,
    pub docs: Option<DocsMeta>,
    pub rpc: Located<String>,
    pub to: Option<Located<String>>,
    pub abi: Option<Located<AbiSpec>>,
    pub function: Option<Located<String>>,
    pub args: Option<Vec<Value>>,
    pub value: Option<Scalar>,
    pub gas: Option<Scalar>,
    pub max_fee: Option<Scalar>,
    pub max_priority_fee: Option<Scalar>,
    /// Legacy transactions only.
    pub gas_price: Option<Scalar>,
    pub nonce: Option<Scalar>,
    #[serde(rename = "type")]
    pub tx_type: Option<TxType>,
    /// The id of a signer in `[evm.signers]`.
    pub signer: Option<String>,
    pub variables: Option<Variables>,
    pub assertions: Option<Vec<Located<Assertion>>>,
    pub captures: Option<Vec<Checked<Capture>>>,
    #[serde(skip)]
    pub source: String,
}

/// The fields both kinds share, borrowed.
#[derive(Clone, Copy, Debug)]
pub struct EvmCommon<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub tags: Option<&'a [String]>,
    pub docs: Option<&'a DocsMeta>,
    pub rpc: &'a Located<String>,
    pub to: Option<&'a Located<String>>,
    pub abi: Option<&'a Located<AbiSpec>>,
    pub function: Option<&'a Located<String>>,
    pub args: &'a [Value],
    pub value: Option<&'a Scalar>,
    pub variables: Option<&'a Variables>,
    pub assertions: &'a [Located<Assertion>],
    pub captures: &'a [Checked<Capture>],
    pub source: &'a str,
}

impl EvmRequest {
    /// Parse and check the `kind: evm-call` or `kind: evm-tx` file `file`
    /// whose text is `text`.
    pub fn parse(file: &Path, text: &str) -> Result<Self, LoadError> {
        #[derive(Deserialize)]
        struct Kind {
            kind: String,
        }
        let Kind { kind } = from_yaml(file, text)?;
        let mut request = if kind == EVM_TX_KIND {
            Self::Tx(from_yaml(file, text)?)
        } else {
            Self::Call(from_yaml(file, text)?)
        };
        request.check(file)?;
        match &mut request {
            Self::Call(call) => call.source = text.to_owned(),
            Self::Tx(tx) => tx.source = text.to_owned(),
        }
        Ok(request)
    }

    /// The load-time checks: `to`, `abi` and `function` are present.
    fn check(&self, file: &Path) -> Result<(), LoadError> {
        let common = self.common();
        let kind = common.kind;
        if common.rpc.trim().is_empty() {
            return Err(LoadError::new(
                file,
                common.rpc.position(),
                format!("{kind}: rpc must not be empty"),
            ));
        }
        let to = common.to.map(|to| to.trim()).unwrap_or_default();
        if to.is_empty() {
            let message = match self {
                Self::Tx(_) => DEPLOYMENT_OUT_OF_SCOPE.to_owned(),
                Self::Call(_) => format!("{kind}: 'to' is required (an address or ENS name)"),
            };
            return Err(LoadError::new(file, None, message));
        }
        if common.abi.is_none() {
            return Err(LoadError::new(
                file,
                None,
                format!("{kind}: 'abi' is required"),
            ));
        }
        if common.function.is_none_or(|f| f.trim().is_empty()) {
            return Err(LoadError::new(
                file,
                None,
                format!("{kind}: 'function' is required"),
            ));
        }
        Ok(())
    }

    /// The fields both kinds share.
    pub fn common(&self) -> EvmCommon<'_> {
        match self {
            Self::Call(r) => EvmCommon {
                kind: &r.kind,
                name: &r.name,
                description: r.description.as_deref(),
                tags: r.tags.as_deref(),
                docs: r.docs.as_ref(),
                rpc: &r.rpc,
                to: r.to.as_ref(),
                abi: r.abi.as_ref(),
                function: r.function.as_ref(),
                args: r.args.as_deref().unwrap_or_default(),
                value: r.value.as_ref(),
                variables: r.variables.as_ref(),
                assertions: r.assertions.as_deref().unwrap_or_default(),
                captures: r.captures.as_deref().unwrap_or_default(),
                source: &r.source,
            },
            Self::Tx(r) => EvmCommon {
                kind: &r.kind,
                name: &r.name,
                description: r.description.as_deref(),
                tags: r.tags.as_deref(),
                docs: r.docs.as_ref(),
                rpc: &r.rpc,
                to: r.to.as_ref(),
                abi: r.abi.as_ref(),
                function: r.function.as_ref(),
                args: r.args.as_deref().unwrap_or_default(),
                value: r.value.as_ref(),
                variables: r.variables.as_ref(),
                assertions: r.assertions.as_deref().unwrap_or_default(),
                captures: r.captures.as_deref().unwrap_or_default(),
                source: &r.source,
            },
        }
    }

    pub fn kind(&self) -> &str {
        self.common().kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<EvmRequest, LoadError> {
        EvmRequest::parse(Path::new("e.rman.yaml"), text)
    }

    const CALL: &str = "version: 1\nkind: evm-call\nname: Balance\nrpc: \"{{rpc}}\"\nto: \"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48\"\nabi: erc20\nfunction: balanceOf\nargs: [\"vitalik.eth\"]\nblock: 19000000\nstate_overrides:\n  \"0x0000000000000000000000000000000000000001\": {balance: \"100 ether\", storage: {\"0x0\": \"0x1\"}}\n";

    #[test]
    fn an_evm_call_loads() {
        let EvmRequest::Call(call) = parse(CALL).unwrap() else {
            panic!("not a call");
        };
        assert_eq!(call.block, Some(Scalar("19000000".into())));
        assert_eq!(
            call.abi.as_ref().map(Located::get),
            Some(&AbiSpec::Source("erc20".into()))
        );
        let overrides = call.state_overrides.unwrap();
        let first = overrides.values().next().unwrap();
        assert_eq!(first.balance, Some(Scalar("100 ether".into())));
        assert_eq!(call.source, CALL);
    }

    #[test]
    fn explorer_abi_and_bad_abi_values() {
        let text = CALL.replace("abi: erc20", "abi: {explorer: true}");
        let request = parse(&text).unwrap();
        assert_eq!(
            request.common().abi.map(Located::get),
            Some(&AbiSpec::Explorer)
        );
        for bad in ["abi: {explorer: false}", "abi: {path: x}", "abi: \"\""] {
            let err = parse(&CALL.replace("abi: erc20", bad)).unwrap_err();
            assert!(err.message.contains("abi must be"), "{bad}: {err}");
        }
    }

    #[test]
    fn unknown_and_tx_only_fields_are_rejected_in_a_call() {
        let err = parse(&format!("{CALL}gas: 100\n")).unwrap_err();
        assert!(err.message.contains("gas"), "{err}");
        let err = parse(&format!("{CALL}signer: me\n")).unwrap_err();
        assert!(err.message.contains("signer"), "{err}");
    }

    #[test]
    fn required_fields() {
        let err = parse(&CALL.replace("to: \"0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48\"\n", ""))
            .unwrap_err();
        assert_eq!(
            err.message,
            "evm-call: 'to' is required (an address or ENS name)"
        );
        let err = parse(&CALL.replace("function: balanceOf\n", "")).unwrap_err();
        assert_eq!(err.message, "evm-call: 'function' is required");
        let err = parse(&CALL.replace("abi: erc20\n", "")).unwrap_err();
        assert_eq!(err.message, "evm-call: 'abi' is required");
    }

    const TX: &str = "version: 1\nkind: evm-tx\nname: Send\nrpc: \"{{rpc}}\"\nto: bob.eth\nabi: erc20\nfunction: transfer\nargs: [bob.eth, \"10\"]\ngas: 100000\nmax_fee: 30 gwei\ntype: eip1559\nsigner: deployer\n";

    #[test]
    fn an_evm_tx_loads_and_needs_to() {
        let EvmRequest::Tx(tx) = parse(TX).unwrap() else {
            panic!("not a tx");
        };
        assert_eq!(tx.tx_type, Some(TxType::Eip1559));
        assert_eq!(tx.max_fee, Some(Scalar("30 gwei".into())));
        let err = parse(&TX.replace("to: bob.eth\n", "")).unwrap_err();
        assert_eq!(err.message, DEPLOYMENT_OUT_OF_SCOPE);
        let err = parse(&format!("{TX}state_overrides: {{}}\n")).unwrap_err();
        assert!(err.message.contains("state_overrides"), "{err}");
        let err = parse(&TX.replace("type: eip1559", "type: blob")).unwrap_err();
        assert!(err.message.contains("blob"), "{err}");
    }

    #[test]
    fn negative_scalars_are_rejected() {
        let err = parse(&CALL.replace("block: 19000000", "block: -1")).unwrap_err();
        assert!(err.message.contains("unsigned"), "{err}");
    }
}
