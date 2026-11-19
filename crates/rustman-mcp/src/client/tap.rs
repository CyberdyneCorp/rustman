//! Records server notifications as the transport delivers them. rmcp runs
//! each notification handler in its own task, so a handler can run after
//! the answer that followed its notification; recording here, in the
//! receive loop, keeps the stream in arrival order and makes every
//! notification sent before an answer part of that call's exchange.

use std::future::Future;
use std::sync::Arc;

use rmcp::RoleClient;
use rmcp::model::{JsonRpcMessage, ServerNotification};
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{IntoTransport, Transport};

use super::handler::ClientState;
use super::notify::{self, NotificationKind};

pub(crate) struct Tap<T> {
    inner: T,
    state: Arc<ClientState>,
}

/// `transport`, recording the notifications it receives into `state`.
pub(crate) fn tap<E, A>(
    transport: impl IntoTransport<RoleClient, E, A>,
    state: Arc<ClientState>,
) -> Tap<impl Transport<RoleClient, Error = E>>
where
    E: std::error::Error + Send + Sync + 'static,
{
    Tap {
        inner: transport.into_transport(),
        state,
    }
}

impl<T: Transport<RoleClient>> Transport<RoleClient> for Tap<T> {
    type Error = T::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleClient>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleClient>> {
        let message = self.inner.receive().await;
        if let Some(JsonRpcMessage::Notification(notification)) = &message {
            let kind = kind_of(&self.state, &notification.notification);
            self.state.notifications.push(kind);
        }
        message
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        self.inner.close()
    }
}

fn kind_of(state: &ClientState, notification: &ServerNotification) -> NotificationKind {
    match notification {
        ServerNotification::CancelledNotification(n) => notify::cancelled_kind(n.params.clone()),
        ServerNotification::ProgressNotification(n) => notify::progress_kind(&n.params),
        ServerNotification::LoggingMessageNotification(n) => notify::log_kind(n.params.clone()),
        ServerNotification::ResourceUpdatedNotification(n) => NotificationKind::ResourceUpdated {
            uri: n.params.uri.clone(),
            subscribed: state.subscribed(&n.params.uri),
        },
        ServerNotification::ResourceListChangedNotification(_) => {
            NotificationKind::ResourcesListChanged
        }
        ServerNotification::ToolListChangedNotification(_) => NotificationKind::ToolsListChanged,
        ServerNotification::PromptListChangedNotification(_) => {
            NotificationKind::PromptsListChanged
        }
        ServerNotification::CustomNotification(n) => NotificationKind::Other {
            method: n.method.clone(),
        },
        _ => NotificationKind::Other {
            method: "notification".to_owned(),
        },
    }
}
