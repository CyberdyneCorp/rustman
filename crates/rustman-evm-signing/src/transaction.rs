//! The transaction that gets signed: EIP-1559 (type 2, the default) or
//! legacy with EIP-155 replay protection. Both embed the chain id.
//!
//! The signed bytes exist only between [`UnsignedTx::sign`] and the send:
//! they are never shown, logged or stored.

use alloy_primitives::{Address, B256, Signature, U256, keccak256};
use alloy_rlp::{Encodable, Header};

use crate::fees::Fees;
use crate::signer::UnlockedSigner;

/// The EIP-2718 type byte of an EIP-1559 transaction.
const EIP1559_TYPE: u8 = 0x02;

/// Everything a transaction commits to, before signing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnsignedTx {
    pub chain_id: u64,
    pub nonce: u64,
    pub to: Address,
    pub value: U256,
    pub input: Vec<u8>,
    pub gas_limit: U256,
    pub fees: Fees,
}

/// A signed transaction: its hash and the bytes for
/// `eth_sendRawTransaction`. It has no `Debug` and no `Display`.
pub struct SignedTx {
    pub hash: B256,
    raw: Vec<u8>,
}

impl SignedTx {
    /// `0x`-prefixed hex of the signed bytes, for the send only.
    pub(crate) fn raw_hex(&self) -> String {
        alloy_primitives::hex::encode_prefixed(&self.raw)
    }
}

impl UnsignedTx {
    /// The hash the signer signs.
    pub fn signing_hash(&self) -> B256 {
        keccak256(self.encode(None))
    }

    /// The most the transaction can cost: value + gas limit × max fee.
    pub fn max_cost(&self) -> U256 {
        self.gas_limit
            .saturating_mul(self.fees.max_per_gas())
            .saturating_add(self.value)
    }

    /// Sign with `signer`.
    pub(crate) fn sign(&self, signer: &UnlockedSigner) -> SignedTx {
        let signature = signer.sign_hash(&self.signing_hash());
        let raw = self.encode(Some(&signature));
        SignedTx {
            hash: keccak256(&raw),
            raw,
        }
    }

    /// The signing payload (no signature) or the signed transaction.
    fn encode(&self, signature: Option<&Signature>) -> Vec<u8> {
        match self.fees {
            Fees::Eip1559 {
                max_fee,
                max_priority_fee,
            } => {
                let mut items = vec![
                    int(U256::from(self.chain_id)),
                    int(U256::from(self.nonce)),
                    int(max_priority_fee.wei),
                    int(max_fee.wei),
                ];
                items.extend(self.common());
                items.push(list(&[])); // empty access list
                if let Some(signature) = signature {
                    items.push(int(U256::from(u8::from(signature.v()))));
                    items.extend(rs(signature));
                }
                let mut out = vec![EIP1559_TYPE];
                out.extend(list(&items));
                out
            }
            Fees::Legacy { gas_price } => {
                let mut items = vec![int(U256::from(self.nonce)), int(gas_price.wei)];
                items.extend(self.common());
                match signature {
                    // EIP-155: v = recovery id + chain id × 2 + 35.
                    Some(signature) => {
                        let v = U256::from(self.chain_id)
                            .saturating_mul(U256::from(2))
                            .saturating_add(U256::from(35 + u64::from(signature.v())));
                        items.push(int(v));
                        items.extend(rs(signature));
                    }
                    None => {
                        items.push(int(U256::from(self.chain_id)));
                        items.push(int(U256::ZERO));
                        items.push(int(U256::ZERO));
                    }
                }
                list(&items)
            }
        }
    }

    /// gas limit, to, value, data: the fields both types share, in order.
    fn common(&self) -> [Vec<u8>; 4] {
        [
            int(self.gas_limit),
            bytes(self.to.as_slice()),
            int(self.value),
            bytes(&self.input),
        ]
    }
}

fn rs(signature: &Signature) -> [Vec<u8>; 2] {
    [int(signature.r()), int(signature.s())]
}

/// An RLP integer: big-endian without leading zeros.
fn int(value: U256) -> Vec<u8> {
    bytes(&value.to_be_bytes_trimmed_vec())
}

fn bytes(value: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    value.encode(&mut out);
    out
}

/// An RLP list of already encoded items.
fn list(items: &[Vec<u8>]) -> Vec<u8> {
    let payload_length = items.iter().map(Vec::len).sum();
    let mut out = Vec::new();
    Header {
        list: true,
        payload_length,
    }
    .encode(&mut out);
    for item in items {
        out.extend_from_slice(item);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fees::Fee;
    use crate::signer::{from_key_bytes, parse_key};

    fn fee(wei: u64) -> Fee {
        Fee {
            wei: U256::from(wei),
            suggested: false,
        }
    }

    /// The example of EIP-155 itself.
    fn eip155_example() -> UnsignedTx {
        UnsignedTx {
            chain_id: 1,
            nonce: 9,
            to: "0x3535353535353535353535353535353535353535"
                .parse()
                .unwrap(),
            value: U256::from(1_000_000_000_000_000_000_u64),
            input: Vec::new(),
            gas_limit: U256::from(21_000),
            fees: Fees::Legacy {
                gas_price: fee(20_000_000_000),
            },
        }
    }

    #[test]
    fn legacy_transactions_follow_the_eip155_example() {
        let tx = eip155_example();
        assert_eq!(
            alloy_primitives::hex::encode(tx.encode(None)),
            "ec098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a764000080018080"
        );
        assert_eq!(
            tx.signing_hash().to_string(),
            "0xdaf5a779ae972f972197303d7b574746c7ef83eadac0f2791ad23db92e4c8e53"
        );
        // The example's key: 0x46 repeated 32 times.
        let key = parse_key(&"46".repeat(32)).unwrap();
        let signer = from_key_bytes("example", &key).unwrap();
        let signed = tx.sign(&signer);
        assert_eq!(
            signed.raw_hex(),
            "0xf86c098504a817c800825208943535353535353535353535353535353535353535880de0b6b3a76400008025a028ef61340bd939bc2195fe537567866003e1a15d3c71ff63e1590620aa636276a067cbe9d8997f761aecb703304b3800ccf555c9f3dc64214b297fb1966a3b6d83"
        );
    }

    #[test]
    fn eip1559_transactions_are_typed_and_commit_to_the_chain() {
        let mut tx = eip155_example();
        tx.fees = Fees::Eip1559 {
            max_fee: fee(30_000_000_000),
            max_priority_fee: fee(1_000_000_000),
        };
        let payload = tx.encode(None);
        assert_eq!(payload[0], EIP1559_TYPE);
        assert_eq!(
            alloy_primitives::hex::encode(&payload),
            "02f00109843b9aca008506fc23ac00825208943535353535353535353535353535353535353535880de0b6b3a764000080c0"
        );
        let other_chain = UnsignedTx {
            chain_id: 5,
            ..tx.clone()
        };
        assert_ne!(tx.signing_hash(), other_chain.signing_hash());
    }

    #[test]
    fn eip1559_signatures_match_an_independent_encoder() {
        // A known vector, not checked against a node: the same transaction
        // made by Foundry offline (`cast mktx 0x3535…3535 --value
        // 1000000000000000000 --nonce 9 --gas-limit 21000 --gas-price
        // 30000000000 --priority-gas-price 1000000000 --chain 1
        // --private-key 0x4646…4646`), the key of the EIP-155 example.
        let mut tx = eip155_example();
        tx.fees = Fees::Eip1559 {
            max_fee: fee(30_000_000_000),
            max_priority_fee: fee(1_000_000_000),
        };
        let key = parse_key(&"46".repeat(32)).unwrap();
        let signed = tx.sign(&from_key_bytes("example", &key).unwrap());
        assert_eq!(
            signed.raw_hex(),
            "0x02f8730109843b9aca008506fc23ac00825208943535353535353535353535353535353535353535880de0b6b3a764000080c001a0ad4241a480b4069450d9bd1823a3afb8bd5a697e2135056f13fc85778785a013a0782c2fc38f01d18de96e4f5caeb077484ef6d844f2ce873a475515552460c075"
        );
        assert_eq!(signed.hash, keccak256(&signed.raw));
    }

    #[test]
    fn the_max_cost_is_the_value_plus_gas_at_the_max_fee() {
        let tx = eip155_example();
        let expected = U256::from(1_000_000_000_000_000_000_u64)
            + U256::from(21_000) * U256::from(20_000_000_000_u64);
        assert_eq!(tx.max_cost(), expected);
    }
}
