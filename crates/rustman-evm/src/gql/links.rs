//! Links from GraphQL results into the EVM toolkit (blockchain-graphql
//! "Links into the EVM decoder"): 20-byte values are addresses, 32-byte
//! values in a field named like a transaction hash are transactions. Each
//! offers what the toolkit can do with it, and the explorer page of the
//! environment's chain.

use serde_json::Value;

use super::format::checksum;

/// Links listed at most per result.
pub const MAX_LINKS: usize = 200;

/// What a value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Address,
    Transaction,
}

/// An actionable value of a result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    pub kind: LinkKind,
    /// The hash, or the checksummed address.
    pub value: String,
    /// Where it was first found: `transfers[0].transactionHash`.
    pub path: String,
}

/// What can be done with a link.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkAction {
    DecodeTransaction,
    ShowBalance,
    CallContract,
    OpenExplorer,
}

impl LinkAction {
    pub fn label(self) -> &'static str {
        match self {
            Self::DecodeTransaction => "Decode transaction",
            Self::ShowBalance => "Show balance",
            Self::CallContract => "Call contract",
            Self::OpenExplorer => "Open in explorer",
        }
    }
}

impl Link {
    /// The actions of this link; the explorer only when the chain has one.
    pub fn actions(&self, explorer: bool) -> Vec<LinkAction> {
        let mut actions = match self.kind {
            LinkKind::Transaction => vec![LinkAction::DecodeTransaction],
            LinkKind::Address => vec![LinkAction::ShowBalance, LinkAction::CallContract],
        };
        if explorer {
            actions.push(LinkAction::OpenExplorer);
        }
        actions
    }

    /// The page of this value on the explorer at `base`.
    pub fn explorer_url(&self, base: &str) -> String {
        let base = base.trim_end_matches('/');
        match self.kind {
            LinkKind::Transaction => format!("{base}/tx/{}", self.value),
            LinkKind::Address => format!("{base}/address/{}", self.value),
        }
    }
}

/// Whether the field `name` holds transaction hashes.
pub fn is_tx_hash_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == "hash" || lower.ends_with("transactionhash") || lower.ends_with("txhash")
}

/// The actionable values of `data`, each value once, in the order its
/// maps iterate.
pub fn find_links(data: &Value) -> Vec<Link> {
    let mut links = Vec::new();
    walk(data, "", None, &mut links);
    links
}

fn walk(value: &Value, path: &str, name: Option<&str>, links: &mut Vec<Link>) {
    if links.len() >= MAX_LINKS {
        return;
    }
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = match path {
                    "" => key.clone(),
                    _ => format!("{path}.{key}"),
                };
                walk(child, &child_path, Some(key), links);
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                walk(item, &format!("{path}[{i}]"), name, links);
            }
        }
        Value::String(text) => {
            if let Some(link) = link(text, path, name)
                && !links.iter().any(|l| l.value == link.value)
            {
                links.push(link);
            }
        }
        _ => {}
    }
}

fn link(text: &str, path: &str, name: Option<&str>) -> Option<Link> {
    let digits = text.strip_prefix("0x")?;
    if !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let (kind, value) = match digits.len() {
        40 => (LinkKind::Address, checksum(text)?),
        64 if name.is_some_and(is_tx_hash_name) => {
            (LinkKind::Transaction, text.to_ascii_lowercase())
        }
        _ => return None,
    };
    Some(Link {
        kind,
        value,
        path: path.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const HASH: &str = "0x5c504ed432cb51138bcf09aa5e8a410dd4a1e204ef84bfed1be16dfba1b22060";
    const ADDRESS: &str = "0xd8da6bf26964af9d7eed9e03e53415d37aa96045";

    #[test]
    fn finds_transactions_and_addresses() {
        let data = json!({"transfers": [
            {"transactionHash": HASH, "from": ADDRESS, "root": HASH},
            {"transactionHash": HASH, "to": ADDRESS}
        ]});
        let links = find_links(&data);
        assert_eq!(links.len(), 2, "{links:?}");
        // Key order follows serde_json's map (sorted, or as written).
        let of = |kind| links.iter().find(|l| l.kind == kind).unwrap();
        let address = of(LinkKind::Address);
        assert_eq!(address.value, "0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045");
        assert_eq!(address.path, "transfers[0].from");
        assert_eq!(
            of(LinkKind::Transaction).path,
            "transfers[0].transactionHash"
        );
    }

    #[test]
    fn actions_and_explorer_urls() {
        let tx = Link {
            kind: LinkKind::Transaction,
            value: HASH.into(),
            path: "hash".into(),
        };
        assert_eq!(tx.actions(false), vec![LinkAction::DecodeTransaction]);
        assert_eq!(
            tx.explorer_url("https://etherscan.io/"),
            format!("https://etherscan.io/tx/{HASH}")
        );
        let address = Link {
            kind: LinkKind::Address,
            ..tx
        };
        let labels: Vec<_> = address.actions(true).iter().map(|a| a.label()).collect();
        assert_eq!(
            labels,
            ["Show balance", "Call contract", "Open in explorer"]
        );
        assert!(is_tx_hash_name("txHash") && !is_tx_hash_name("blockRoot"));
    }
}
