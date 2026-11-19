//! The gRPC status check (add-grpc-client "Assertions and captures"):
//! `grpc_status: <name|number>` passes when the call ended with that code.

use super::{AssertionResult, Check, ResponseView};
use crate::model::GrpcStatusExpectation;

/// Why `grpc_status` cannot be evaluated against a response.
const NOT_GRPC: &str = "grpc_status: not a gRPC response";

/// Why `metadata` cannot be evaluated against a response.
pub(super) const NOT_GRPC_METADATA: &str = "metadata: not a gRPC response";

pub(super) fn evaluate(
    expected: GrpcStatusExpectation,
    response: &ResponseView<'_>,
) -> AssertionResult {
    let name = expected.name();
    let check = Check::new("grpc_status", format!("grpc_status {name}"));
    let Some(grpc) = &response.grpc else {
        return check.error(NOT_GRPC);
    };
    check.verdict(
        grpc.code == expected.code,
        "grpc_status",
        name.to_owned(),
        grpc.label(),
    )
}
