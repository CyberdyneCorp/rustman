//! Method guards and allowlist presets: the two registries other
//! capabilities extend (spec "Method guards", "Allowlist presets").
//!
//! A [`MethodGuard`] sees every call before it is sent, from every face
//! (`rustman rpc`, `rustman run`, the TUI and the agent tool), because the
//! one send path ([`crate::JsonrpcRunner`]) and the agent policy
//! ([`crate::agent`]) both ask [`check_target`] first. A refusal stops the
//! whole request with the guard's message (exit code 4), whatever the
//! request file or `agent_read_only_methods` say.
//!
//! A preset names a list of method globs (`@evm-read`) that
//! `agent_read_only_methods` entries can refer to.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock, RwLock};

use rustman_core::vars::ResolvedJsonrpc;
use serde_json::Value;

/// Something that may refuse to let a method be sent.
pub trait MethodGuard: Send + Sync {
    /// The refusal message for calling `method` with `params`; `None`
    /// lets the call through.
    fn check(&self, method: &str, params: Option<&Value>) -> Option<String>;
}

/// A guard refused a call: nothing is sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct GuardRefusal {
    /// The refused method.
    pub method: String,
    /// The guard's own message.
    pub message: String,
}

static GUARDS: LazyLock<RwLock<Vec<Arc<dyn MethodGuard>>>> =
    LazyLock::new(|| RwLock::new(Vec::new()));

static PRESETS: LazyLock<RwLock<BTreeMap<String, Vec<String>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

/// Add `guard` to the guards every send asks. Guards stay registered for
/// the life of the process.
pub fn register_guard(guard: Arc<dyn MethodGuard>) {
    if let Ok(mut guards) = GUARDS.write() {
        guards.push(guard);
    }
}

/// Remove `guard` (the same `Arc` given to [`register_guard`]), so a test
/// leaves no guard behind for the tests after it.
pub fn unregister_guard(guard: &Arc<dyn MethodGuard>) {
    if let Ok(mut guards) = GUARDS.write() {
        guards.retain(|g| !Arc::ptr_eq(g, guard));
    }
}

/// The first refusal of any guard for `method` with `params`.
pub fn check_method(method: &str, params: Option<&Value>) -> Option<GuardRefusal> {
    let guards = GUARDS.read().ok()?;
    guards.iter().find_map(|guard| {
        guard.check(method, params).map(|message| GuardRefusal {
            method: method.to_owned(),
            message,
        })
    })
}

/// The first refusal for any method `target` may send: its calls and,
/// for a subscription, the `subscribe` method with the call's params and
/// the `unsubscribe` method (its params, the subscription id, are not
/// known yet: the guards see none).
pub fn check_target(target: &ResolvedJsonrpc) -> Option<GuardRefusal> {
    let calls = target
        .calls
        .iter()
        .map(|c| (c.method.as_str(), c.params.as_ref()));
    let params = target.calls.first().and_then(|c| c.params.as_ref());
    let teardown = target.subscription.iter().flat_map(|spec| {
        [
            (spec.subscribe.as_str(), params),
            (spec.unsubscribe.as_str(), None),
        ]
    });
    check_calls(calls.chain(teardown))
}

/// The first refusal for any of `calls` (method and params).
pub fn check_calls<'a>(
    calls: impl IntoIterator<Item = (&'a str, Option<&'a Value>)>,
) -> Option<GuardRefusal> {
    calls
        .into_iter()
        .find_map(|(method, params)| check_method(method, params))
}

/// Register the preset `name` (with or without its leading `@`) as the
/// method globs `patterns`, replacing an earlier one of that name.
pub fn register_preset(name: &str, patterns: &[&str]) {
    let name = name.trim_start_matches('@').to_owned();
    let patterns = patterns.iter().map(|p| (*p).to_owned()).collect();
    if let Ok(mut presets) = PRESETS.write() {
        presets.insert(name, patterns);
    }
}

/// The globs of the preset `name` (without its `@`).
pub fn preset(name: &str) -> Option<Vec<String>> {
    PRESETS.read().ok()?.get(name).cloned()
}

/// The registered preset names, each with its `@`, sorted.
pub fn preset_names() -> Vec<String> {
    PRESETS
        .read()
        .map(|p| p.keys().map(|k| format!("@{k}")).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Refuse(&'static str);

    impl MethodGuard for Refuse {
        fn check(&self, method: &str, _params: Option<&Value>) -> Option<String> {
            (method == self.0).then(|| format!("{method} is guarded"))
        }
    }

    #[test]
    fn a_registered_guard_refuses_only_its_method() {
        let guard: Arc<dyn MethodGuard> = Arc::new(Refuse("guard_unit_send"));
        register_guard(guard.clone());
        let refused = check_method("guard_unit_send", None).unwrap();
        assert_eq!(refused.message, "guard_unit_send is guarded");
        assert_eq!(refused.to_string(), "guard_unit_send is guarded");
        assert!(check_method("guard_unit_read", Some(&json!([]))).is_none());
        let calls = [("guard_unit_read", None), ("guard_unit_send", None)];
        assert_eq!(check_calls(calls).unwrap().method, "guard_unit_send");
        unregister_guard(&guard);
        assert!(check_method("guard_unit_send", None).is_none(), "removed");
    }

    #[test]
    fn a_subscription_target_asks_the_guards_about_unsubscribe() {
        // Regression: the unsubscribe call was sent without asking the guards.
        use rustman_core::model::{RequestSettings, SubscriptionSpec};
        use rustman_core::vars::{
            Resolved, ResolvedBody, ResolvedCall, ResolvedJsonrpc, ResolvedRequest, TlsFiles,
        };
        let guard: Arc<dyn MethodGuard> = Arc::new(Refuse("guard_unit_unsubscribe"));
        register_guard(guard.clone());
        let call = ResolvedCall::new("guard_unit_subscribe", Some(json!(["heads"])));
        let request = ResolvedRequest {
            method: "POST".to_owned(),
            url: Resolved::literal("ws://127.0.0.1:1/"),
            query: Vec::new(),
            headers: Vec::new(),
            body: ResolvedBody::None,
            auth: None,
            settings: RequestSettings::default(),
            tls: TlsFiles::default(),
            secrets: Vec::new(),
        };
        let mut target = ResolvedJsonrpc::new(request, vec![call], false);
        assert!(check_target(&target).is_none());
        target.subscription = Some(SubscriptionSpec {
            subscribe: "guard_unit_subscribe".into(),
            unsubscribe: "guard_unit_unsubscribe".into(),
            notification: "guard_unit_notification".into(),
            notification_id_path: None,
            max_events: None,
            max_duration_ms: None,
        });
        let refused = check_target(&target).unwrap();
        assert_eq!(refused.method, "guard_unit_unsubscribe");
        unregister_guard(&guard);
    }

    #[test]
    fn presets_are_registered_by_name() {
        register_preset("@guard-unit-read", &["guard_unit_get*"]);
        assert_eq!(
            preset("guard-unit-read").unwrap(),
            vec!["guard_unit_get*".to_owned()]
        );
        assert!(preset_names().contains(&"@guard-unit-read".to_owned()));
        assert!(preset("nope").is_none());
    }
}
