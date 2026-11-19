//! The standard health check (spec "Health check helper"): one call to
//! `grpc.health.v1.Health/Check`, whose messages are small enough to be
//! declared here instead of discovered.

use std::fmt;
use std::time::Duration;

use http::uri::PathAndQuery;
use tonic::client::Grpc;
use tonic_prost::ProstCodec;

use crate::connect::Remote;
use crate::error::GrpcError;
use crate::status::{DEADLINE_EXCEEDED, GrpcStatus};

/// The method the health check calls.
pub const HEALTH_CHECK: &str = "/grpc.health.v1.Health/Check";

/// `grpc.health.v1.HealthCheckRequest`.
#[derive(Clone, PartialEq, prost::Message)]
struct HealthCheckRequest {
    #[prost(string, tag = "1")]
    service: String,
}

/// `grpc.health.v1.HealthCheckResponse`.
#[derive(Clone, PartialEq, prost::Message)]
struct HealthCheckResponse {
    #[prost(int32, tag = "1")]
    status: i32,
}

/// The serving status a health service reports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServingStatus {
    Unknown,
    Serving,
    NotServing,
    ServiceUnknown,
}

impl ServingStatus {
    fn of(status: i32) -> Self {
        match status {
            1 => Self::Serving,
            2 => Self::NotServing,
            3 => Self::ServiceUnknown,
            _ => Self::Unknown,
        }
    }

    /// `SERVING`, `NOT_SERVING`, `SERVICE_UNKNOWN` or `UNKNOWN`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "UNKNOWN",
            Self::Serving => "SERVING",
            Self::NotServing => "NOT_SERVING",
            Self::ServiceUnknown => "SERVICE_UNKNOWN",
        }
    }
}

impl fmt::Display for ServingStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// How a health check ended.
#[derive(Clone, Debug, PartialEq)]
pub enum HealthCheck {
    /// The health service answered.
    Status(ServingStatus),
    /// The call failed with a status, such as `UNIMPLEMENTED` from a
    /// server without a health service.
    Failed(GrpcStatus),
}

/// Ask `remote` how `service` is doing (`""`: the whole server), within
/// `deadline`. Fails when the server cannot be reached.
pub async fn check_health(
    remote: &Remote,
    service: &str,
    deadline: Duration,
) -> Result<HealthCheck, GrpcError> {
    let channel = remote.channel().await?;
    let mut grpc = Grpc::new(channel);
    grpc.ready().await.map_err(|e| GrpcError::Transport {
        target: remote.target.text.clone(),
        message: e.to_string(),
    })?;
    let mut request = tonic::Request::new(HealthCheckRequest {
        service: service.to_owned(),
    });
    request.set_timeout(deadline);
    let path = PathAndQuery::from_static(HEALTH_CHECK);
    let codec = ProstCodec::<HealthCheckRequest, HealthCheckResponse>::default();
    let call = grpc.unary(request, path, codec);
    let Ok(answer) = tokio::time::timeout(deadline, call).await else {
        let status = GrpcStatus::new(DEADLINE_EXCEEDED, "deadline exceeded");
        return Ok(HealthCheck::Failed(status));
    };
    Ok(match answer {
        Ok(response) => HealthCheck::Status(ServingStatus::of(response.into_inner().status)),
        Err(status) => HealthCheck::Failed(GrpcStatus::from_tonic(&status)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_have_their_proto_names() {
        let names: Vec<&str> = (0..5).map(|s| ServingStatus::of(s).name()).collect();
        assert_eq!(
            names,
            [
                "UNKNOWN",
                "SERVING",
                "NOT_SERVING",
                "SERVICE_UNKNOWN",
                "UNKNOWN"
            ]
        );
        assert_eq!(ServingStatus::Serving.to_string(), "SERVING");
    }
}
