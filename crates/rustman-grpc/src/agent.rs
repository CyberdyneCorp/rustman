//! What the agent tools `grpc_list`, `grpc_describe` and `grpc_call` may
//! reach (spec "Agent tools").
//!
//! A gRPC target is checked by the safety policy as the `http` or `https`
//! origin it is reached over (`grpc://h:1` as `http://h:1`, `grpcs://h:1`
//! as `https://h:1`), before any traffic: reflection included. Every call
//! is admitted and paced by the session's governor, so the rate and
//! concurrency limits apply across all the session's requests. Only unary
//! and server-streaming methods may be called; response streams stop at
//! the request's `stream` bounds, as under `rustman run`.
//!
//! gRPC has no read verbs: every call is an HTTP/2 `POST`. A method is a
//! read only when its proto says so (`option idempotency_level =
//! NO_SIDE_EFFECTS;`); any other method is a mutation, which the tool lets
//! through only under the agent's mutation permission and the human's
//! confirmation ([`AgentGrpc::confirm`]). The runner refuses an
//! unconfirmed mutation again before sending, whatever the tool did.

use std::sync::{Arc, Mutex, PoisonError};

use rustman_core::safety::{Admission, Blocked};
use rustman_core::vars::ResolvedGrpc;
use rustman_http::runner::{GrpcCall, GrpcExecutor, GrpcFailure, GrpcFuture};
use rustman_http::{GovernedSender, SendOptions};

use prost_reflect::MethodDescriptor;

use crate::browse::{CallType, has_no_side_effects};
use crate::connect::Remote;
use crate::error::GrpcError;
use crate::run::{GrpcRunner, remote, schema_for};
use crate::schema::{DescriptorCache, Schema};

/// The names the tools are registered under.
pub const LIST_TOOL: &str = "grpc_list";
pub const DESCRIBE_TOOL: &str = "grpc_describe";
pub const CALL_TOOL: &str = "grpc_call";

/// The message of a client-streaming or bidi method asked for by an agent.
pub const STREAMING_REFUSED: &str =
    "only unary and server-streaming methods are available to agents";

/// The message of a mutation sent without the human's confirmation.
pub const NOT_CONFIRMED: &str = "mutations not allowed for agents";

/// What calling a method does, as far as the agent permissions go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Marked `idempotency_level = NO_SIDE_EFFECTS`.
    Read,
    /// Anything else.
    Mutation,
}

impl Access {
    pub fn of(method: &MethodDescriptor) -> Self {
        if has_no_side_effects(method) {
            Self::Read
        } else {
            Self::Mutation
        }
    }
}

/// Why an agent call was refused; nothing was sent.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AgentRefusal {
    /// The safety policy refused the target.
    #[error(transparent)]
    Policy(#[from] Blocked),
    /// A client-streaming or bidi method.
    #[error("{STREAMING_REFUSED} ({method} is {call_type})")]
    CallType { method: String, call_type: CallType },
    /// A method not marked free of side effects, not confirmed.
    #[error("{NOT_CONFIRMED} ({method} is not marked idempotency_level = NO_SIDE_EFFECTS)")]
    Mutation { method: String },
}

/// Why an agent could not read a target's schema.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Refused(#[from] AgentRefusal),
    /// The governor stopped the session (a budget or a halt).
    #[error("{0}")]
    Halted(String),
    #[error(transparent)]
    Grpc(#[from] GrpcError),
}

/// The `http` or `https` origin the safety policy checks for `remote`.
pub fn origin(remote: &Remote) -> String {
    let scheme = if remote.tls { "https" } else { "http" };
    format!("{scheme}://{}", remote.target.authority())
}

/// The gRPC face of the agent session: its sender's policy and governor.
pub struct AgentGrpc {
    sender: Arc<GovernedSender>,
    runner: GrpcRunner,
    refused: Mutex<Option<AgentRefusal>>,
    /// The mutation the human confirmed (`package.Service/Method`).
    confirmed: Option<String>,
}

impl AgentGrpc {
    pub fn new(sender: Arc<GovernedSender>) -> Self {
        Self {
            sender,
            runner: GrpcRunner::default(),
            refused: Mutex::new(None),
            confirmed: None,
        }
    }

    /// Let the mutation `method` through: the agent may mutate and the
    /// human confirmed it.
    pub fn confirm(&mut self, method: &str) {
        self.confirmed = Some(method.to_owned());
    }

    /// The first call this runner refused; nothing of it was sent.
    pub fn refused(&self) -> Option<AgentRefusal> {
        self.refused
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn refuse(&self, refusal: AgentRefusal) -> AgentRefusal {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        refused.get_or_insert(refusal.clone());
        refusal
    }

    /// Refused unless the policy allows `remote`'s origin.
    pub fn check(&self, remote: &Remote) -> Result<(), AgentRefusal> {
        self.sender
            .policy()
            .check_origin(&origin(remote))
            .map_err(|blocked| self.refuse(blocked.into()))
    }

    /// Wait for a slot and the rate limit; the slot is held until dropped.
    async fn govern(&self) -> Result<Admission, String> {
        let governor = self.sender.governor();
        let admission = governor.admit().await.map_err(|e| e.to_string())?;
        governor.pace().await.map_err(|e| e.to_string())?;
        Ok(admission)
    }

    /// The schema of `resolved` for `grpc_list` and `grpc_describe`:
    /// checked and paced like a call, since reflection reaches the server.
    pub async fn schema(
        &self,
        resolved: &ResolvedGrpc,
        options: &SendOptions,
        cache: Option<DescriptorCache>,
        refresh: bool,
    ) -> Result<Schema, AgentError> {
        self.check(&remote(resolved, options)?)?;
        let _slot = self.govern().await.map_err(AgentError::Halted)?;
        let (schema, _) = schema_for(resolved, options, cache, refresh).await?;
        Ok(schema)
    }

    /// What calling the method of `resolved` does, read from its schema
    /// (checked and paced like a call); client-streaming and bidi methods
    /// are refused.
    pub async fn access(
        &self,
        resolved: &ResolvedGrpc,
        options: &SendOptions,
        cache: Option<DescriptorCache>,
    ) -> Result<Access, AgentError> {
        let schema = self.schema(resolved, options, cache, false).await?;
        let method = schema.method(&resolved.method)?;
        Ok(self.admit(&resolved.method, &method)?)
    }

    /// Refuse client-streaming and bidi methods.
    fn admit(&self, path: &str, method: &MethodDescriptor) -> Result<Access, AgentRefusal> {
        let call_type = CallType::of(method);
        if !matches!(call_type, CallType::Unary | CallType::ServerStreaming) {
            return Err(self.refuse(AgentRefusal::CallType {
                method: path.to_owned(),
                call_type,
            }));
        }
        Ok(Access::of(method))
    }

    /// Refuse what [`Self::admit`] refuses, and unconfirmed mutations.
    fn check_method(&self, path: &str, method: &MethodDescriptor) -> Result<(), AgentRefusal> {
        let access = self.admit(path, method)?;
        if access == Access::Mutation && self.confirmed.as_deref() != Some(path) {
            return Err(self.refuse(AgentRefusal::Mutation {
                method: path.to_owned(),
            }));
        }
        Ok(())
    }
}

impl GrpcExecutor for AgentGrpc {
    fn execute<'a>(&'a self, call: GrpcCall<'a>) -> GrpcFuture<'a> {
        Box::pin(async move {
            let refused = |message: String| GrpcFailure {
                message,
                config: false,
            };
            let remote = remote(call.resolved, call.options).map_err(|e| GrpcFailure {
                message: e.redacted(&call.resolved.redactor()).to_string(),
                config: true,
            })?;
            self.check(&remote).map_err(|r| refused(r.to_string()))?;
            let _slot = self.govern().await.map_err(refused)?;
            let method = call.resolved.method.clone();
            let check = |descriptor: &MethodDescriptor| {
                self.check_method(&method, descriptor)
                    .map_err(|r| GrpcError::Message(r.to_string()))
            };
            self.runner.run(call, check).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_are_checked_as_their_http_origin() {
        let plain = Remote::parse("grpc://127.0.0.1:50051").unwrap();
        assert_eq!(origin(&plain), "http://127.0.0.1:50051");
        let tls = Remote::parse("grpcs://api.example.com:443").unwrap();
        assert_eq!(origin(&tls), "https://api.example.com:443");
        let bare = Remote::parse("api.example.com:443").unwrap();
        assert_eq!(origin(&bare), "https://api.example.com:443");
    }

    /// A method of the test catalog; `strip` drops the methods' options,
    /// so its reads become mutations.
    fn catalog_method(name: &str, strip: bool) -> MethodDescriptor {
        use prost::Message;
        let bytes = rustman_test_support::grpc::descriptor_set();
        let mut set = prost_types::FileDescriptorSet::decode(bytes.as_slice()).unwrap();
        let methods = set.file.iter_mut().flat_map(|f| f.service.iter_mut());
        for method in methods.flat_map(|s| s.method.iter_mut()) {
            if strip {
                method.options = None;
            }
        }
        let pool = prost_reflect::DescriptorPool::from_file_descriptor_set(set).unwrap();
        let service = pool.get_service_by_name("shop.v1.Catalog").unwrap();
        service.methods().find(|m| m.name() == name).unwrap()
    }

    fn agent() -> AgentGrpc {
        use rustman_core::safety::{
            EnvironmentInfo, Governor, Limits, Permission, PolicyOptions, SafetyPolicy,
            SafetySettings,
        };
        let safety = SafetySettings::default();
        let options = PolicyOptions {
            target: "http://127.0.0.1:1",
            permission: Permission::Mutations,
            allow_hosts: &[],
            production: None,
            environment: EnvironmentInfo::default(),
        };
        let policy = SafetyPolicy::new(&safety, options).unwrap();
        let governor = Governor::new(Limits::from_settings(&safety));
        AgentGrpc::new(Arc::new(GovernedSender::new(policy, governor)))
    }

    const GET: &str = "shop.v1.Catalog/GetProduct";

    #[test]
    fn only_methods_marked_free_of_side_effects_are_reads() {
        assert_eq!(
            Access::of(&catalog_method("GetProduct", false)),
            Access::Read
        );
        assert_eq!(
            Access::of(&catalog_method("ListProducts", false)),
            Access::Read
        );
        assert_eq!(
            Access::of(&catalog_method("GetProduct", true)),
            Access::Mutation
        );
    }

    #[test]
    fn a_mutation_is_refused_unless_that_method_was_confirmed() {
        let mutation = catalog_method("GetProduct", true);
        let mut agent = agent();
        let refused = agent.check_method(GET, &mutation).unwrap_err();
        assert_eq!(
            refused.to_string(),
            "mutations not allowed for agents (shop.v1.Catalog/GetProduct is not marked idempotency_level = NO_SIDE_EFFECTS)"
        );
        assert_eq!(agent.refused(), Some(refused));
        agent.confirm("shop.v1.Catalog/Other");
        assert!(
            agent.check_method(GET, &mutation).is_err(),
            "another method"
        );
        agent.confirm(GET);
        assert_eq!(agent.check_method(GET, &mutation), Ok(()));
        let read = catalog_method("GetProduct", false);
        assert_eq!(agent.check_method(GET, &read), Ok(()));
    }

    #[test]
    fn a_confirmed_bidi_method_stays_refused() {
        let path = "shop.v1.Catalog/Chat";
        let mut agent = agent();
        agent.confirm(path);
        let chat = catalog_method("Chat", false);
        assert!(matches!(
            agent.check_method(path, &chat),
            Err(AgentRefusal::CallType { .. })
        ));
    }

    #[test]
    fn a_streaming_refusal_names_the_method() {
        let refusal = AgentRefusal::CallType {
            method: "shop.v1.Catalog/Chat".into(),
            call_type: CallType::Bidi,
        };
        assert_eq!(
            refusal.to_string(),
            "only unary and server-streaming methods are available to agents (shop.v1.Catalog/Chat is bidi)"
        );
    }
}
