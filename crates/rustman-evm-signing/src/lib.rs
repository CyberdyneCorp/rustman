//! The signing gate of the EVM toolkit (add-evm-toolkit "evm-signing",
//! design D2): the only code in Rustman that may unlock a key.
//!
//! - [`config`]: `[evm.signers.<id>]`, keystore, keychain or hardware
//!   wallet; a raw private key anywhere is refused ([`rawkey`]).
//! - [`signer`]: unlocking a signer; key material lives in `Secret` and
//!   `Zeroizing` holders only.
//! - [`import`]: `rustman evm signer import`, the one way a key gets in.
//! - [`gate`]: the preconditions (workspace opt-in, `--sign`, chain pin,
//!   chain match, simulation), each refusing with exit code 4.
//! - [`fees`] and [`nonce`]: suggested fees, the gas limit and nonces.
//! - [`confirm`]: the summary the user confirms and the [`Confirmation`]
//!   the send step requires.
//! - [`transaction`] and [`send`]: the one place a transaction is signed
//!   and sent, then the receipt wait.
//! - [`receipt`]: the decoded receipt, its assertions and the audit record.
//! - [`flow`]: one `evm-tx` through all of the above, for `rustman evm
//!   send` and `rustman run --sign` (the run's executor).
//!
//! `rustman-mcp` does not depend on this crate, so agents cannot reach a
//! signer at all.

pub mod config;
pub mod confirm;
pub mod display;
pub mod fees;
pub mod flow;
pub mod gate;
pub mod import;
pub mod nonce;
pub mod rawkey;
pub mod receipt;
pub mod send;
pub mod signer;
pub mod transaction;

pub use alloy_primitives::B256;
pub use config::{SignerConfig, SignerSource, Signers};
pub use confirm::{Confirmation, Mode, Summary};
pub use flow::{Console, Ending, SignOptions, Signing};
pub use rawkey::{RAW_KEY_REFUSED, is_raw_key, refuse_raw_key};
pub use signer::{UnlockContext, UnlockedSigner, unlock};
pub use zeroize::Zeroizing;
