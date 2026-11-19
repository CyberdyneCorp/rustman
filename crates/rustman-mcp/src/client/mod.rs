//! Rustman as an MCP client (mcp-client, add-mcp-client): connect to the
//! MCP servers a workspace defines, over stdio, Streamable HTTP or legacy
//! HTTP+SSE, initialize a session, and use it: tools, resources, prompts
//! and completion, notifications, progress and cancellation, and
//! server-initiated requests through an approval broker. Shares the pinned `rmcp` with
//! [`crate::server`]; the SDK stays behind [`McpSession`].

// rmcp marks roots, sampling, logging and resource subscriptions deprecated
// (SEP-2577, the 2026-07-28 revision), but they are part of every revision
// the client negotiates (see `version::SUPPORTED`).
#![allow(deprecated)]

pub mod broker;

/// The MCP protocol models (tools, resources, prompts, sampling), for
/// front-ends that show them.
pub use rmcp::model as protocol;

pub mod definition;
pub mod error;
pub mod handler;
mod http;
pub mod info;
mod legacy;
mod line_limit;
pub mod notify;
pub mod oauth;
pub mod operation;
pub mod output;
mod process_tree;
pub mod prompts;
pub mod request;
pub mod resolve;
pub mod resources;
pub mod roots;
pub mod run;
pub mod schema;
pub mod session;
mod shared;
mod sse_limit;
pub mod stdio;
mod tap;
pub mod target;
pub mod tools;
pub mod trust;
pub mod version;

pub use broker::{
    Accepted, Approval, ApprovalBroker, ElicitationDecision, ElicitationRequest, HeadlessBroker,
    InteractiveBroker, Reply, SamplingDecision, SamplingRequest, load_sampling_response,
};
pub use definition::{
    HttpDefinition, OAuthClient, SERVERS_DIR, ServerDefinition, StdioDefinition,
    TransportDefinition, find_server, load_servers,
};
pub use error::ClientError;
pub use handler::{CLIENT_NAME, RustmanClient};
pub use info::{Capabilities, Disabled, Operation, SessionInfo, TransportKind};
pub use notify::{Notification, NotificationKind, Notifications};
pub use operation::{McpOp, OpResult};
pub use process_tree::kill_all_servers;
pub use prompts::{CompletionTarget, Debouncer, PromptLine, PromptResult};
pub use request::{DEFAULT_TIMEOUT, Exchange, Progress, RequestOptions};
pub use resolve::{
    ResolvedHttp, ResolvedOAuthClient, ResolvedServer, ResolvedStdio, ResolvedTransport,
};
pub use resources::{ResourceContent, expand_template, template_variables};
pub use run::McpRunner;
pub use schema::{Field, FieldKind, Violation};
pub use session::{ConnectOptions, McpSession, SessionStatus};
pub use stdio::{ProcessExit, SHUTDOWN_GRACE, STDERR_TAIL, StderrLog};
pub use target::ServerTarget;
pub use tools::{CallOptions, ContentItem, ToolResult};
pub use trust::{Approved, PendingApproval, TrustCheck, approve_typed, check_trust, require_trust};
