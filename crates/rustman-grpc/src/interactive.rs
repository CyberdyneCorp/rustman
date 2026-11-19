//! Interactive calls (spec "Streaming calls", "Message timeline"): a front
//! end such as the TUI watches the timeline of a running call as it
//! happens, and keeps a bidi stream open to send further messages typed
//! by the user before half-closing it.

use std::sync::{Mutex, PoisonError};

use prost_reflect::{DynamicMessage, MessageDescriptor};
use rustman_core::Redactor;
use rustman_core::model::GrpcTransport;
use serde_json::Value;
use tokio::sync::mpsc;

use crate::browse::CallType;
use crate::call::{CallOptions, LiveFeed, PreparedCall};
use crate::error::GrpcError;
use crate::exchange::{GrpcExchange, TimelineEntry};
use crate::json::to_message;

/// What an interactive call is fed by and reports to.
#[derive(Debug, Default)]
pub struct Interactive {
    /// Messages (proto3 JSON) the user sends while a bidi call runs, after
    /// the request's own; the call half-closes when every sender is
    /// dropped. Taken by the first bidi call.
    outbound: Mutex<Option<mpsc::UnboundedReceiver<Value>>>,
    /// Every timeline entry as it happens, redacted.
    feed: Option<mpsc::UnboundedSender<TimelineEntry>>,
}

impl Interactive {
    pub fn new(
        outbound: Option<mpsc::UnboundedReceiver<Value>>,
        feed: Option<mpsc::UnboundedSender<TimelineEntry>>,
    ) -> Self {
        Self {
            outbound: Mutex::new(outbound),
            feed,
        }
    }

    fn take_outbound(&self) -> Option<mpsc::UnboundedReceiver<Value>> {
        self.outbound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Send `call`: its own messages, then (bidi only) what the user sends
    /// until the outbound channel closes. gRPC-Web calls are sent as is.
    pub async fn send(
        &self,
        call: &PreparedCall,
        options: &CallOptions,
        redactor: &Redactor,
    ) -> Result<GrpcExchange, GrpcError> {
        if call.transport == GrpcTransport::GrpcWeb {
            return call.send(options).await;
        }
        let typed = (call.call_type == CallType::Bidi)
            .then(|| self.take_outbound())
            .flatten();
        let (tx, rx) = mpsc::channel(call.messages.len().max(1));
        for message in &call.messages {
            // The channel holds every message: this cannot fail.
            let _ = tx.try_send(message.clone());
        }
        let live = self.feed.clone().map(|entries| LiveFeed {
            entries,
            redactor: redactor.clone(),
        });
        let input = call.method.input();
        let forward = async move {
            if let Some(typed) = typed {
                forward(typed, &input, &tx).await;
            }
            drop(tx);
            std::future::pending::<()>().await;
        };
        tokio::select! {
            sent = call.send_live(options, rx, live) => sent,
            () = forward => unreachable!("forwarding never ends by itself"),
        }
    }
}

/// Encode and send each typed message until the user half-closes (the
/// sender is dropped) or the call ends. A message the schema rejects is
/// skipped: front ends check typed messages before sending them.
async fn forward(
    mut typed: mpsc::UnboundedReceiver<Value>,
    input: &MessageDescriptor,
    tx: &mpsc::Sender<DynamicMessage>,
) {
    while let Some(value) = typed.recv().await {
        if let Ok(message) = to_message(input, &value)
            && tx.send(message).await.is_err()
        {
            return;
        }
    }
}
