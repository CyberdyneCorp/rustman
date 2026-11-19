//! A tonic codec for `prost_reflect::DynamicMessage` (design D1): requests
//! and responses are encoded from descriptors found at runtime, so no
//! per-service generated code is needed.

use prost::Message as _;
use prost_reflect::{DynamicMessage, MessageDescriptor};
use tonic::Status;
use tonic::codec::{Codec, DecodeBuf, Decoder, EncodeBuf, Encoder};

/// Encodes any [`DynamicMessage`] and decodes messages of one type.
#[derive(Clone, Debug)]
pub struct DynamicCodec {
    response: MessageDescriptor,
}

impl DynamicCodec {
    /// A codec decoding responses of type `response`.
    pub fn new(response: MessageDescriptor) -> Self {
        Self { response }
    }
}

impl Codec for DynamicCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynamicEncoder;
    type Decoder = DynamicDecoder;

    fn encoder(&mut self) -> DynamicEncoder {
        DynamicEncoder
    }

    fn decoder(&mut self) -> DynamicDecoder {
        DynamicDecoder(self.response.clone())
    }
}

/// The encoding half of [`DynamicCodec`].
#[derive(Clone, Copy, Debug)]
pub struct DynamicEncoder;

impl Encoder for DynamicEncoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn encode(&mut self, item: DynamicMessage, dst: &mut EncodeBuf<'_>) -> Result<(), Status> {
        item.encode(dst)
            .map_err(|e| Status::internal(format!("cannot encode message: {e}")))
    }
}

/// The decoding half of [`DynamicCodec`].
#[derive(Clone, Debug)]
pub struct DynamicDecoder(MessageDescriptor);

impl Decoder for DynamicDecoder {
    type Item = DynamicMessage;
    type Error = Status;

    fn decode(&mut self, src: &mut DecodeBuf<'_>) -> Result<Option<DynamicMessage>, Status> {
        DynamicMessage::decode(self.0.clone(), src)
            .map(Some)
            .map_err(|e| Status::internal(format!("cannot decode {}: {e}", self.0.full_name())))
    }
}
