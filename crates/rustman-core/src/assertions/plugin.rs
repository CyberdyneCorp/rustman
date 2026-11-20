//! `plugin: {fn: <plugin>/<fn>, ...args}`: an assertion function of a
//! plugin (plugins spec, "Assertion extension point"). The plugin gets the
//! exchange and the args; its verdict is reported like a built-in
//! assertion, and a plugin fault (trap, limit) is an assertion `error`.

use super::response::Parsed;
use super::{AssertionResult, Check, Outcome};
use crate::model::PluginAssertion;
use crate::plugins::{Extensions, PluginExchange, PluginVerdict};

/// What plugin assertions need: the loaded plugins, and the request side
/// of the exchange (redacted).
pub struct PluginCalls<'r> {
    pub extensions: &'r dyn Extensions,
    pub request: PluginExchange,
}

pub(super) fn evaluate(
    assertion: &PluginAssertion,
    response: &Parsed<'_, '_>,
    calls: Option<&PluginCalls<'_>>,
) -> AssertionResult {
    let function = &assertion.function;
    let check = Check::new("plugin", format!("plugin {function}"));
    let Some(calls) = calls else {
        let plugin = function.split('/').next().unwrap_or_default();
        return check.error(format!(
            "plugin {plugin} is not loaded (the workspace plugins are not trusted, or this build has no plugin support)"
        ));
    };
    let view = response.view;
    let exchange = PluginExchange {
        status: view.status,
        response_headers: view.headers.clone(),
        response_body: view.body.to_vec(),
        duration_ms: u64::try_from(view.total.as_millis()).unwrap_or(u64::MAX),
        ..calls.request.clone()
    };
    let args = serde_json::Value::Object(assertion.args.clone()).to_string();
    match calls.extensions.check(function, &exchange, &args) {
        PluginVerdict::Pass => check.pass(),
        PluginVerdict::Fail(message) => {
            check.result(Outcome::Fail, None, None, format!("{function}: {message}"))
        }
        PluginVerdict::Error(message) if message.starts_with("plugin ") => check.error(message),
        PluginVerdict::Error(message) => check.error(format!("{function}: {message}")),
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::time::Duration;

    use super::*;
    use crate::assertions::ResponseView;
    use crate::plugins::{SignOutput, SignRequest};

    struct Fixed(PluginVerdict);

    impl Extensions for Fixed {
        fn generate(&self, _: &str, _: &str, _: &[String]) -> Result<String, String> {
            unreachable!()
        }
        fn sign(&self, _: &str, _: &SignRequest) -> Result<SignOutput, String> {
            unreachable!()
        }
        fn check(&self, function: &str, exchange: &PluginExchange, args: &str) -> PluginVerdict {
            assert_eq!(function, "shop/price-consistent");
            assert_eq!(exchange.status, 200);
            assert_eq!(exchange.method, "GET");
            assert_eq!(exchange.response_body, b"{}");
            assert_eq!(args, r#"{"tolerance":0}"#);
            self.0.clone()
        }
    }

    fn run(verdict: Option<PluginVerdict>) -> AssertionResult {
        let view = ResponseView {
            status: 200,
            headers: Vec::new(),
            body: Cow::Borrowed(b"{}"),
            binary: false,
            ttfb: Duration::ZERO,
            total: Duration::from_millis(5),
            grpc: None,
        };
        let parsed = Parsed::new(&view);
        let mut args = serde_json::Map::new();
        args.insert("tolerance".into(), 0.into());
        let assertion = PluginAssertion {
            function: "shop/price-consistent".into(),
            args,
        };
        let fixed = verdict.map(Fixed);
        let calls = fixed.as_ref().map(|f| PluginCalls {
            extensions: f,
            request: PluginExchange {
                method: "GET".into(),
                ..PluginExchange::default()
            },
        });
        evaluate(&assertion, &parsed, calls.as_ref())
    }

    #[test]
    fn verdicts_read_like_built_in_assertions() {
        assert!(run(Some(PluginVerdict::Pass)).passed());
        let fail = run(Some(PluginVerdict::Fail("total 99.0 != sum 98.5".into())));
        assert_eq!(fail.outcome, Outcome::Fail);
        assert_eq!(
            fail.message,
            "shop/price-consistent: total 99.0 != sum 98.5"
        );
        let fault = run(Some(PluginVerdict::Error(
            "plugin shop: execution time limit exceeded".into(),
        )));
        assert_eq!(
            fault.to_string(),
            "error: plugin shop: execution time limit exceeded"
        );
        let error = run(Some(PluginVerdict::Error("no total".into())));
        assert_eq!(error.to_string(), "error: shop/price-consistent: no total");
    }

    #[test]
    fn without_plugins_the_assertion_errors() {
        let result = run(None);
        assert_eq!(result.outcome, Outcome::Error);
        assert!(
            result.message.starts_with("plugin shop is not loaded"),
            "{}",
            result.message
        );
    }
}
