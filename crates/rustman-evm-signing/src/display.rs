//! What the gate shows besides the confirmation fields: the preview
//! printed before `--sign` is checked, and the decoded call with ERC-20
//! amounts in token units.
//!
//! Every value is shown on its own line: newlines, tabs and other control,
//! bidi and zero-width characters are escaped ([`printable_line`]), so a
//! resolved `to` or argument cannot draw lines that look like other
//! fields.

use alloy_json_abi::Function;
use rustman_core::text::printable_line;
use rustman_evm::codec::{DecodedCall, NamedValue};
use rustman_evm::tx::TokenMeta;
use serde_json::Value;

/// The ERC-20 functions that take a token amount, and its input index.
const TOKEN_AMOUNT_INPUTS: [(&str, usize); 3] = [
    ("transfer(address,uint256)", 1),
    ("approve(address,uint256)", 1),
    ("transferFrom(address,address,uint256)", 2),
];

/// The index of the token amount `function` takes, for the ERC-20
/// functions that move or approve one.
pub fn token_amount_input(function: &Function) -> Option<usize> {
    let signature = function.signature();
    TOKEN_AMOUNT_INPUTS
        .iter()
        .find(|(known, _)| *known == signature)
        .map(|(_, index)| *index)
}

/// `transfer(to=0x…, amount=10.00 USDC (10000000))`: with `token`, the
/// amount at its index is shown in token units when the token's decimals
/// are known. The raw integer stays in parentheses, since the decimals
/// and symbol come from the node.
pub fn render_call(call: &DecodedCall, token: Option<(usize, &TokenMeta)>) -> String {
    let parts: Vec<String> = call
        .args
        .iter()
        .enumerate()
        .map(|(i, arg)| match (token, arg.value.as_uint()) {
            (Some((index, meta)), Some((amount, _))) if index == i && meta.decimals.is_some() => {
                labelled(arg, &format!("{} ({amount})", meta.format(amount)))
            }
            _ => arg.to_string(),
        })
        .collect();
    format!("{}({})", call.function.name, parts.join(", "))
}

fn labelled(arg: &NamedValue, shown: &str) -> String {
    match arg.name.is_empty() {
        true => shown.to_owned(),
        false => format!("{}={shown}", arg.name),
    }
}

/// What would be signed, before any node is called or any signer looked
/// up.
pub struct Preview<'a> {
    /// The request's id.
    pub id: &'a str,
    pub to: &'a str,
    pub function: &'a str,
    pub args: &'a [Value],
    pub value: Option<&'a str>,
    /// What [`signer_label`] says.
    pub signer: &'a str,
}

impl Preview<'_> {
    /// The preview, each value passed through `redact` and kept on its
    /// line.
    pub fn render(&self, redact: impl Fn(&str) -> String) -> String {
        let line = |text: &str| printable_line(&redact(text)).into_owned();
        let args: Vec<String> = self.args.iter().map(compact).collect();
        let call = format!("{}({})", self.function, args.join(", "));
        format!(
            "evm-tx {}\n  to      {}\n  call    {}\n  value   {}\n  signer  {}",
            line(self.id),
            line(self.to),
            line(&call),
            line(self.value.unwrap_or("0")),
            line(self.signer),
        )
    }
}

/// The signer the preview names: the id when it is configured in
/// `[evm.signers]`, never anything else (an unknown value may be key
/// material).
pub fn signer_label(named: Option<&str>, configured: impl Fn(&str) -> bool) -> &str {
    match named {
        None => "(none)",
        Some(id) if configured(id) => id,
        Some(_) => "(not a configured signer)",
    }
}

/// An argument as written: strings bare, other values as JSON.
fn compact(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::U256;
    use rustman_evm::codec::decode_input;
    use serde_json::json;

    use super::*;

    #[test]
    fn previews_show_arguments_as_written() {
        assert_eq!(compact(&Value::String("bob.eth".into())), "bob.eth");
        assert_eq!(compact(&json!(10)), "10");
    }

    #[test]
    fn preview_values_cannot_draw_lines() {
        // Regression: the preview escaped with `printable`, which keeps
        // newlines, so a resolved `to` or argument drew fake lines.
        let args = [json!("x\n  value   0\n  signer  deployer")];
        let preview = Preview {
            id: "transfer",
            to: "0xabc\n  call    nothing()",
            function: "transfer",
            args: &args,
            value: Some("1\t"),
            signer: "(none)",
        };
        let text = preview.render(str::to_owned);
        assert_eq!(text.lines().count(), 5, "{text}");
        assert!(text.contains("0xabc\\u{a}  call"), "{text}");
        assert!(text.contains("  value   1\\u{9}\n"), "{text}");
        let redacted = preview.render(|t| t.replace("0xabc", "«redacted»"));
        assert!(redacted.contains("«redacted»"), "{redacted}");
    }

    #[test]
    fn only_a_configured_signer_is_named() {
        // Regression: any value passed as the signer was echoed.
        let configured = |id: &str| id == "deployer";
        assert_eq!(signer_label(Some("deployer"), configured), "deployer");
        assert_eq!(
            signer_label(Some("test test junk"), configured),
            "(not a configured signer)"
        );
        assert_eq!(signer_label(None, configured), "(none)");
    }

    #[test]
    fn token_amounts_show_in_token_units_with_the_raw_amount() {
        // Scenario "Confirmation content": `amount=10.00 USDC`.
        let abi = rustman_evm::abi::built_in("erc20").unwrap();
        let transfer = abi.function("transfer").unwrap()[0].clone();
        assert_eq!(token_amount_input(&transfer), Some(1));
        let to = alloy_primitives::Address::repeat_byte(0x11);
        let mut input = transfer.selector().to_vec();
        input.extend([0_u8; 12]);
        input.extend(to.as_slice());
        input.extend(U256::from(10_000_000_u64).to_be_bytes::<32>());
        let call = decode_input(&abi, &input).unwrap();
        let usdc = TokenMeta {
            decimals: Some(6),
            symbol: Some("USDC".to_owned()),
        };
        let shown = render_call(&call, Some((1, &usdc)));
        let expected = format!(
            "transfer(to={}, amount=10.00 USDC (10000000))",
            to.to_checksum(None)
        );
        assert_eq!(shown, expected);
        let unknown = TokenMeta::default();
        assert_eq!(render_call(&call, Some((1, &unknown))), call.to_string());
        assert_eq!(render_call(&call, None), call.to_string());
    }
}
