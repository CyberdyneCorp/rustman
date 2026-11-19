//! The agent permissions (spec "Read-only agent mode by default" and "Safety
//! policy on every agent request"): method gating from `[agent]`, and the
//! session's [`GovernedSender`], whose allowlist is `[agent].allowed_hosts`
//! or else the hosts of the active environment.

use std::time::Duration;

use rustman_core::Environment;
use rustman_core::agent::AgentSettings;
use rustman_core::model::VarValue;
use rustman_core::safety::{
    EnvironmentInfo, Governor, Limits, MethodClass, Origin, Permission, PolicyOptions,
    SafetyPolicy, SafetySettings,
};
use rustman_http::GovernedSender;

use super::error::ToolError;

/// Refuse `method` unless `permission` allows it, with the reasons agents see.
pub fn check_method(permission: Permission, method: &str) -> Result<(), ToolError> {
    let class = MethodClass::of(method);
    if permission.allows(class) {
        return Ok(());
    }
    Err(ToolError::blocked(match class {
        MethodClass::Mutation => "mutations not allowed for agents",
        _ => "destructive not allowed for agents",
    }))
}

/// The origins agents may reach: `[agent].allowed_hosts`, or else every
/// `http(s)` origin among the active environment's variable values.
pub fn allowlist(agent: &AgentSettings, environment: Option<&Environment>) -> Vec<Origin> {
    if !agent.allowed_hosts.is_empty() {
        return agent.allowed_hosts.clone();
    }
    let mut origins: Vec<Origin> = Vec::new();
    let values = environment.into_iter().flat_map(|e| e.variables.iter());
    for (_, value) in values {
        let VarValue::String(text) = value else {
            continue;
        };
        if let Ok(origin) = Origin::of(text)
            && !origins.contains(&origin)
        {
            origins.push(origin);
        }
    }
    origins
}

/// The limits of a session: the `[safety]` rate and concurrency, shared by
/// every request of the session, without the exploration budgets (a session
/// may stay open for days).
pub fn session_limits(safety: &SafetySettings) -> Limits {
    Limits {
        max_probes: u64::MAX,
        max_duration: Duration::MAX,
        ..Limits::from_settings(safety)
    }
}

/// `[safety]` as agents get it: its limits, with `allowed` (the agent's
/// own allowlist) in place of `[safety].allowed_hosts`, which is for
/// requests and explorations started by a human.
pub fn agent_safety(safety: &SafetySettings, allowed: &[Origin]) -> SafetySettings {
    SafetySettings {
        allowed_hosts: allowed.to_vec(),
        ..safety.clone()
    }
}

/// Build the session's sender. `Err` carries the reason every request is
/// refused: no allowed origin at all, or a production environment (agents
/// cannot confirm production hosts).
pub fn session_sender(
    safety: &SafetySettings,
    agent: &AgentSettings,
    environment: Option<&Environment>,
) -> Result<GovernedSender, String> {
    let policy = policy_with(safety, agent, environment, agent.permission())?;
    Ok(GovernedSender::new(
        policy,
        Governor::new(session_limits(safety)),
    ))
}

/// The sender of the `rpc_call` tool: the session's allowlist and
/// production rule, and the governor of `session` (one rate and
/// concurrency limit for every request of the session), but every
/// JSON-RPC call is a `POST`, so the method rule is
/// `agent_read_only_methods` (checked by the tool) instead of the HTTP
/// method (add-jsonrpc-client design D7).
pub fn rpc_sender(
    session: &GovernedSender,
    safety: &SafetySettings,
    agent: &AgentSettings,
    environment: Option<&Environment>,
) -> Result<GovernedSender, String> {
    let policy = policy_with(safety, agent, environment, Permission::Mutations)?;
    Ok(session.with_policy(policy))
}

fn policy_with(
    safety: &SafetySettings,
    agent: &AgentSettings,
    environment: Option<&Environment>,
    permission: Permission,
) -> Result<SafetyPolicy, String> {
    let origins = allowlist(agent, environment);
    let Some((first, rest)) = origins.split_first() else {
        return Err("host not allowed".to_owned());
    };
    let target = first.to_string();
    let options = PolicyOptions {
        target: &target,
        permission,
        allow_hosts: rest,
        production: None,
        environment: environment.map(EnvironmentInfo::from).unwrap_or_default(),
    };
    let settings = agent_safety(safety, &[]);
    SafetyPolicy::new(&settings, options).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use rustman_core::model::Variables;

    use super::*;

    fn agent(mutations: bool, destructive: bool) -> AgentSettings {
        AgentSettings {
            allow_mutations: mutations,
            allow_destructive: destructive,
            ..AgentSettings::default()
        }
    }

    fn env(name: &str, vars: &[(&str, &str)], production: bool) -> Environment {
        let vars = vars
            .iter()
            .map(|(k, v)| ((*k).to_owned(), VarValue::String((*v).to_owned())))
            .collect();
        Environment {
            name: name.to_owned(),
            variables: Variables::new(vars),
            secrets: Vec::new(),
            production,
        }
    }

    fn outcome(settings: &AgentSettings, method: &str) -> Result<(), String> {
        check_method(settings.permission(), method).map_err(|e| e.message)
    }

    #[test]
    fn every_permission_combination_gates_methods() {
        let mutations = Err("mutations not allowed for agents".to_owned());
        let destructive = Err("destructive not allowed for agents".to_owned());
        let rows = [
            // (allow_mutations, allow_destructive, GET, POST, DELETE, PURGE)
            (false, false, Ok(()), mutations.clone(), destructive.clone()),
            (true, false, Ok(()), Ok(()), destructive.clone()),
            (false, true, Ok(()), Ok(()), Ok(())),
            (true, true, Ok(()), Ok(()), Ok(())),
        ];
        for (m, d, get, post, delete) in rows {
            let settings = agent(m, d);
            assert_eq!(outcome(&settings, "GET"), get, "{m} {d}");
            assert_eq!(outcome(&settings, "HEAD"), Ok(()), "{m} {d}");
            assert_eq!(outcome(&settings, "OPTIONS"), Ok(()), "{m} {d}");
            assert_eq!(outcome(&settings, "POST"), post, "{m} {d}");
            assert_eq!(outcome(&settings, "PUT"), post, "{m} {d}");
            assert_eq!(outcome(&settings, "PATCH"), post, "{m} {d}");
            assert_eq!(outcome(&settings, "DELETE"), delete, "{m} {d}");
            assert_eq!(outcome(&settings, "PURGE"), delete, "{m} {d}");
        }
    }

    #[test]
    fn the_allowlist_falls_back_to_environment_hosts() {
        let local = env(
            "local",
            &[
                ("base_url", "http://127.0.0.1:8080/api"),
                ("auth_url", "https://auth.example.com"),
                ("same", "http://127.0.0.1:8080/other"),
                ("name", "alice"),
            ],
            false,
        );
        let hosts: Vec<String> = allowlist(&AgentSettings::default(), Some(&local))
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            hosts,
            ["http://127.0.0.1:8080", "https://auth.example.com:443"]
        );
        let configured = AgentSettings {
            allowed_hosts: vec![Origin::parse("http://localhost:9000").unwrap()],
            ..AgentSettings::default()
        };
        let hosts = allowlist(&configured, Some(&local));
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].to_string(), "http://localhost:9000");
        assert!(allowlist(&AgentSettings::default(), None).is_empty());
    }

    #[test]
    fn the_sender_allows_only_the_allowlist() {
        let local = env("local", &[("base_url", "http://127.0.0.1:8080")], false);
        let safety = SafetySettings {
            allowed_hosts: vec![Origin::parse("https://elsewhere.example.com").unwrap()],
            ..SafetySettings::default()
        };
        let sender = session_sender(&safety, &AgentSettings::default(), Some(&local)).unwrap();
        let policy = sender.policy();
        assert!(policy.check("GET", "http://127.0.0.1:8080/users").is_ok());
        let refused = policy
            .check("GET", "https://elsewhere.example.com/")
            .unwrap_err();
        assert_eq!(refused.reason(), "host not allowed");
        assert!(policy.check("POST", "http://127.0.0.1:8080/users").is_err());
        assert_eq!(sender.governor().limits().max_probes, u64::MAX);
    }

    #[test]
    fn rpc_call_shares_the_session_governor() {
        // Regression: `rpc_call` had its own governor, so a session got the
        // `[safety]` rate and concurrency twice.
        let local = env("local", &[("base_url", "http://127.0.0.1:8080")], false);
        let (safety, agent) = (SafetySettings::default(), AgentSettings::default());
        let session = session_sender(&safety, &agent, Some(&local)).unwrap();
        let rpc = rpc_sender(&session, &safety, &agent, Some(&local)).unwrap();
        assert!(std::ptr::eq(session.governor(), rpc.governor()));
        assert!(rpc.policy().check("POST", "http://127.0.0.1:8080/").is_ok());
        assert!(
            session
                .policy()
                .check("POST", "http://127.0.0.1:8080/")
                .is_err()
        );
    }

    #[test]
    fn agent_policies_drop_the_human_allowlist() {
        let human = Origin::parse("https://internal.example.com").unwrap();
        let agent = Origin::parse("http://127.0.0.1:8081").unwrap();
        let safety = SafetySettings {
            allowed_hosts: vec![human],
            ..SafetySettings::default()
        };
        let settings = agent_safety(&safety, std::slice::from_ref(&agent));
        assert_eq!(settings.allowed_hosts, [agent]);
        assert_eq!(settings.rate, safety.rate);
        let options = PolicyOptions {
            target: "http://127.0.0.1:8080",
            permission: Permission::ReadOnly,
            allow_hosts: &[],
            production: None,
            environment: EnvironmentInfo::default(),
        };
        let policy = SafetyPolicy::new(&settings, options).unwrap();
        assert!(policy.check("GET", "http://127.0.0.1:8081/a").is_ok());
        let refused = policy
            .check("GET", "https://internal.example.com/metadata")
            .unwrap_err();
        assert_eq!(refused.reason(), "host not allowed");
    }

    #[test]
    fn no_hosts_or_a_production_environment_refuse_every_request() {
        let none = session_sender(&SafetySettings::default(), &AgentSettings::default(), None);
        assert_eq!(none.unwrap_err(), "host not allowed");
        let prod = env("prod", &[("base_url", "https://api.example.com")], true);
        let refused = session_sender(
            &SafetySettings::default(),
            &AgentSettings::default(),
            Some(&prod),
        )
        .unwrap_err();
        assert!(refused.contains("production host"), "{refused}");
    }
}
