//! The server reflection client (design D4): `grpc.reflection.v1`, falling
//! back to `v1alpha` when v1 is `UNIMPLEMENTED`. Services are listed, the
//! file declaring each is fetched, and dependencies are followed
//! transitively into a [`DescriptorPool`].
//!
//! Both versions use the same messages on the wire, so one client speaks
//! both: only the method path differs.

use std::collections::BTreeMap;

use http::uri::PathAndQuery;
use prost::Message as _;
use prost_reflect::DescriptorPool;
use prost_types::{FileDescriptorProto, FileDescriptorSet};
use tonic::client::Grpc;
use tonic::transport::Channel;
use tonic::{Code, Status};
use tonic_prost::ProstCodec;
use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
use tonic_reflection::pb::v1::server_reflection_response::MessageResponse;
use tonic_reflection::pb::v1::{ServerReflectionRequest, ServerReflectionResponse};

use crate::error::GrpcError;

/// The reflection method of each version, tried in this order.
const PATHS: [&str; 2] = [
    "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo",
    "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo",
];

/// The most files reflection may fetch for one schema.
const MAX_FILES: usize = 4_096;

/// The most encoded descriptor bytes reflection may fetch for one schema.
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Discover the schema of the server behind `channel`; `target` names it
/// in errors.
pub async fn reflect(channel: Channel, target: &str) -> Result<DescriptorPool, GrpcError> {
    for path in PATHS {
        let mut client = Reflector {
            grpc: Grpc::new(channel.clone()),
            path,
            target,
        };
        match client.list_services().await {
            Ok(services) => return client.pool(&services).await,
            Err(status) if status.code() == Code::Unimplemented => continue,
            Err(status) => return Err(client.failed(&status.to_string())),
        }
    }
    Err(GrpcError::ReflectionUnavailable(target.to_owned()))
}

struct Reflector<'a> {
    grpc: Grpc<Channel>,
    path: &'static str,
    target: &'a str,
}

impl Reflector<'_> {
    fn failed(&self, message: &str) -> GrpcError {
        GrpcError::Reflection {
            target: self.target.to_owned(),
            message: message.to_owned(),
        }
    }

    /// One request on its own reflection stream, and its answer.
    async fn ask(&mut self, request: MessageRequest) -> Result<MessageResponse, Status> {
        let request = ServerReflectionRequest {
            host: String::new(),
            message_request: Some(request),
        };
        self.grpc
            .ready()
            .await
            .map_err(|e| Status::unavailable(e.to_string()))?;
        let codec = ProstCodec::<ServerReflectionRequest, ServerReflectionResponse>::default();
        let response = self
            .grpc
            .streaming(
                tonic::Request::new(tokio_stream::iter([request])),
                PathAndQuery::from_static(self.path),
                codec,
            )
            .await?;
        let answer = response.into_inner().message().await?;
        answer
            .and_then(|a| a.message_response)
            .ok_or_else(|| Status::internal("empty reflection response"))
    }

    async fn list_services(&mut self) -> Result<Vec<String>, Status> {
        match self
            .ask(MessageRequest::ListServices(String::new()))
            .await?
        {
            MessageResponse::ListServicesResponse(list) => {
                Ok(list.service.into_iter().map(|s| s.name).collect())
            }
            other => Err(unexpected(&other)),
        }
    }

    /// The files of an answer, decoded.
    async fn files(&mut self, request: MessageRequest) -> Result<Vec<FileDescriptorProto>, Status> {
        match self.ask(request).await? {
            MessageResponse::FileDescriptorResponse(found) => found
                .file_descriptor_proto
                .iter()
                .map(|bytes| FileDescriptorProto::decode(bytes.as_slice()))
                .collect::<Result<_, _>>()
                .map_err(|e| Status::internal(format!("invalid file descriptor: {e}"))),
            other => Err(unexpected(&other)),
        }
    }

    /// The pool of every listed service the server can describe, with
    /// every dependency of their files.
    async fn pool(&mut self, services: &[String]) -> Result<DescriptorPool, GrpcError> {
        let mut fetched = Fetched::default();
        for service in services {
            let symbol = MessageRequest::FileContainingSymbol(service.clone());
            // A listed service without descriptors (such as a health service
            // registered apart from reflection) is left out of the schema.
            if let Ok(found) = self.files(symbol).await {
                fetched.add(found).map_err(|e| self.failed(&e))?;
            }
        }
        while let Some(missing) = fetched.missing_dependency() {
            let found = self.dependency(&missing).await?;
            fetched.add(found).map_err(|e| self.failed(&e))?;
        }
        let set = FileDescriptorSet {
            file: fetched.files.into_values().collect(),
        };
        DescriptorPool::from_file_descriptor_set(set).map_err(|e| self.failed(&e.to_string()))
    }

    /// A dependency by file name; well-known types the server does not
    /// serve come from the bundled `google/protobuf` files.
    async fn dependency(&mut self, name: &str) -> Result<Vec<FileDescriptorProto>, GrpcError> {
        let request = MessageRequest::FileByFilename(name.to_owned());
        match self.files(request).await {
            Ok(found) if found.iter().any(|f| f.name() == name) => Ok(found),
            _ => bundled(name)
                .map(|file| vec![file])
                .ok_or_else(|| self.failed(&format!("dependency '{name}' not found"))),
        }
    }
}

/// The files fetched so far, within limits: a server that names new
/// dependencies without end cannot make the client grow without bound.
struct Fetched {
    files: BTreeMap<String, FileDescriptorProto>,
    bytes: usize,
    max_files: usize,
    max_bytes: usize,
}

impl Default for Fetched {
    fn default() -> Self {
        Self::with_limits(MAX_FILES, MAX_BYTES)
    }
}

impl Fetched {
    fn with_limits(max_files: usize, max_bytes: usize) -> Self {
        Self {
            files: BTreeMap::new(),
            bytes: 0,
            max_files,
            max_bytes,
        }
    }

    /// Keep the files of `found` not fetched yet; fails past the limits.
    fn add(&mut self, found: Vec<FileDescriptorProto>) -> Result<(), String> {
        for file in found {
            if self.files.contains_key(file.name()) {
                continue;
            }
            self.bytes += file.encoded_len();
            self.files.insert(file.name().to_owned(), file);
        }
        if self.files.len() > self.max_files || self.bytes > self.max_bytes {
            return Err(format!(
                "schema too large (more than {} files or {} bytes)",
                self.max_files, self.max_bytes
            ));
        }
        Ok(())
    }

    /// The first dependency not fetched yet.
    fn missing_dependency(&self) -> Option<String> {
        self.files
            .values()
            .flat_map(|f| &f.dependency)
            .find(|d| !self.files.contains_key(d.as_str()))
            .cloned()
    }
}

/// A well-known `google/protobuf/*.proto` file from prost-reflect's pool.
fn bundled(name: &str) -> Option<FileDescriptorProto> {
    DescriptorPool::global()
        .get_file_by_name(name)
        .map(|f| f.file_descriptor_proto().clone())
}

fn unexpected(response: &MessageResponse) -> Status {
    match response {
        MessageResponse::ErrorResponse(e) => {
            Status::new(Code::from(e.error_code), &e.error_message)
        }
        _ => Status::internal("unexpected reflection response"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, dependency: &[&str]) -> FileDescriptorProto {
        FileDescriptorProto {
            name: Some(name.to_owned()),
            dependency: dependency.iter().map(|d| (*d).to_owned()).collect(),
            ..FileDescriptorProto::default()
        }
    }

    #[test]
    fn dependencies_are_followed_until_none_is_missing() {
        let mut fetched = Fetched::default();
        fetched.add(vec![file("a.proto", &["b.proto"])]).unwrap();
        assert_eq!(fetched.missing_dependency().as_deref(), Some("b.proto"));
        fetched
            .add(vec![file("b.proto", &[]), file("a.proto", &[])])
            .unwrap();
        assert_eq!(fetched.missing_dependency(), None);
        assert_eq!(fetched.files.len(), 2);
    }

    #[test]
    fn an_endless_dependency_chain_stops_at_the_file_limit() {
        let mut fetched = Fetched::with_limits(3, MAX_BYTES);
        let mut result = Ok(());
        for n in 0..10 {
            let next = format!("f{}.proto", n + 1);
            result = fetched.add(vec![file(&format!("f{n}.proto"), &[&next])]);
            if result.is_err() {
                break;
            }
        }
        let err = result.unwrap_err();
        assert!(err.starts_with("schema too large"), "{err}");
        assert_eq!(fetched.files.len(), 4);
    }

    #[test]
    fn oversized_descriptors_stop_at_the_byte_limit() {
        let mut fetched = Fetched::with_limits(MAX_FILES, 64);
        fetched.add(vec![file("small.proto", &[])]).unwrap();
        let big = file(&"x".repeat(100), &[]);
        assert!(fetched.add(vec![big]).is_err());
    }

    #[test]
    fn the_default_limits() {
        let fetched = Fetched::default();
        assert_eq!((fetched.max_files, fetched.max_bytes), (4_096, 64 << 20));
    }
}
