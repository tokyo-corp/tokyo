use serde::{Deserialize, Serialize};

use crate::auth::AuthRequirement;
use crate::http::Parameter;
use crate::id::ChannelId;
use crate::types::TypeRef;

/// A persistent WebSocket channel, as opposed to a request/response [`crate::http::Endpoint`].
///
/// Channels have no HTTP method or request/response body; they describe a
/// long-lived connection exchanging typed messages in one or both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebSocketChannel {
    /// Stable channel identifier.
    pub id: ChannelId,
    /// Rust-safe channel name.
    pub name: String,
    /// OpenAPI path template.
    pub path: String,
    /// Path-item-level server URL, after substituting declared defaults.
    #[serde(default)]
    pub server_url: Option<String>,
    /// Short channel summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// Longer channel documentation.
    pub docs: Option<String>,
    /// OpenAPI tags used for grouping.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Path template parameters.
    pub path_parameters: Vec<Parameter>,
    /// Query parameters used while connecting.
    pub query_parameters: Vec<Parameter>,
    /// Ordered OpenAPI security alternatives (OR), applied at connect time.
    /// An empty outer list means the channel is public.
    #[serde(default)]
    pub auth: Vec<AuthRequirement>,
    /// Message direction and payload types.
    pub direction: WebSocketDirection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Direction and payload types for a WebSocket channel.
pub enum WebSocketDirection {
    /// Client sends messages to the server.
    ClientToServer {
        /// Outbound message type.
        send: TypeRef,
    },
    /// Client receives messages from the server.
    ServerToClient {
        /// Inbound message type.
        receive: TypeRef,
    },
    /// Client both sends and receives typed messages.
    Bidirectional {
        /// Outbound message type.
        send: TypeRef,
        /// Inbound message type.
        receive: TypeRef,
    },
}

impl WebSocketDirection {
    /// Returns the outbound message type when this channel supports sending.
    #[must_use]
    pub fn send(&self) -> Option<&TypeRef> {
        match self {
            Self::ClientToServer { send } | Self::Bidirectional { send, .. } => Some(send),
            Self::ServerToClient { .. } => None,
        }
    }

    /// Returns the inbound message type when this channel supports receiving.
    #[must_use]
    pub fn receive(&self) -> Option<&TypeRef> {
        match self {
            Self::ServerToClient { receive } | Self::Bidirectional { receive, .. } => Some(receive),
            Self::ClientToServer { .. } => None,
        }
    }
}
