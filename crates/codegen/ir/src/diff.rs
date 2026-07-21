use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::api::Api;
use crate::id::{ChannelId, EndpointId, TypeId};

/// A single change between two `Api` snapshots. This is the core primitive the
/// CI-native sync loop is built on: diff the IR from the last generation against
/// the current inputs, and every `Change` becomes one concise notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Change {
    /// CLI behavior changed.
    CliBehaviorChanged,
    /// SDK behavior changed.
    SdkBehaviorChanged,
    /// Omitted OpenAPI construct counts changed.
    OmissionsChanged,
    /// Retained OpenAPI component schemas changed.
    SchemaComponentsChanged,
    /// A type declaration was added.
    TypeAdded(TypeId),
    /// A type declaration was removed.
    TypeRemoved(TypeId),
    /// A type declaration changed.
    TypeChanged(TypeId),
    /// An endpoint was added.
    EndpointAdded(EndpointId),
    /// An endpoint was removed.
    EndpointRemoved(EndpointId),
    /// An endpoint changed.
    EndpointChanged(EndpointId),
    /// A WebSocket channel was added.
    ChannelAdded(ChannelId),
    /// A WebSocket channel was removed.
    ChannelRemoved(ChannelId),
    /// A WebSocket channel changed.
    ChannelChanged(ChannelId),
}

/// Diffs two `Api` snapshots at type/endpoint granularity and reports coarse
/// target-behavior changes. Field-level detail can be layered on later without
/// changing the existing variants.
#[must_use = "diff results should be inspected or reported"]
pub fn diff_api_snapshots(previous_api_snapshot: &Api, current_api_snapshot: &Api) -> Vec<Change> {
    let mut detected_changes = Vec::new();

    if previous_api_snapshot.cli != current_api_snapshot.cli {
        detected_changes.push(Change::CliBehaviorChanged);
    }
    if previous_api_snapshot.sdk != current_api_snapshot.sdk {
        detected_changes.push(Change::SdkBehaviorChanged);
    }
    if previous_api_snapshot.omissions != current_api_snapshot.omissions {
        detected_changes.push(Change::OmissionsChanged);
    }
    if previous_api_snapshot.schema_components != current_api_snapshot.schema_components {
        detected_changes.push(Change::SchemaComponentsChanged);
    }

    let previous_type_declarations_by_id: BTreeMap<&TypeId, &crate::types::TypeDeclaration> =
        previous_api_snapshot
            .types
            .iter()
            .map(|type_declaration| (&type_declaration.id, type_declaration))
            .collect();
    let current_type_declarations_by_id: BTreeMap<&TypeId, &crate::types::TypeDeclaration> =
        current_api_snapshot
            .types
            .iter()
            .map(|type_declaration| (&type_declaration.id, type_declaration))
            .collect();

    for (type_id, current_type_declaration) in &current_type_declarations_by_id {
        match previous_type_declarations_by_id.get(type_id) {
            None => detected_changes.push(Change::TypeAdded((*type_id).clone())),
            Some(previous_type_declaration) => {
                if previous_type_declaration != current_type_declaration {
                    detected_changes.push(Change::TypeChanged((*type_id).clone()));
                }
            }
        }
    }
    for type_id in previous_type_declarations_by_id.keys() {
        if !current_type_declarations_by_id.contains_key(*type_id) {
            detected_changes.push(Change::TypeRemoved((*type_id).clone()));
        }
    }

    let previous_endpoints_by_id: BTreeMap<&EndpointId, &crate::http::Endpoint> =
        previous_api_snapshot
            .endpoints
            .iter()
            .map(|endpoint| (&endpoint.id, endpoint))
            .collect();
    let current_endpoints_by_id: BTreeMap<&EndpointId, &crate::http::Endpoint> =
        current_api_snapshot
            .endpoints
            .iter()
            .map(|endpoint| (&endpoint.id, endpoint))
            .collect();

    for (endpoint_id, current_endpoint) in &current_endpoints_by_id {
        match previous_endpoints_by_id.get(endpoint_id) {
            None => detected_changes.push(Change::EndpointAdded((*endpoint_id).clone())),
            Some(previous_endpoint) => {
                if previous_endpoint != current_endpoint {
                    detected_changes.push(Change::EndpointChanged((*endpoint_id).clone()));
                }
            }
        }
    }
    for endpoint_id in previous_endpoints_by_id.keys() {
        if !current_endpoints_by_id.contains_key(*endpoint_id) {
            detected_changes.push(Change::EndpointRemoved((*endpoint_id).clone()));
        }
    }

    let previous_channels_by_id: BTreeMap<&ChannelId, &crate::websocket::WebSocketChannel> =
        previous_api_snapshot
            .channels
            .iter()
            .map(|channel| (&channel.id, channel))
            .collect();
    let current_channels_by_id: BTreeMap<&ChannelId, &crate::websocket::WebSocketChannel> =
        current_api_snapshot
            .channels
            .iter()
            .map(|channel| (&channel.id, channel))
            .collect();

    for (channel_id, current_channel) in &current_channels_by_id {
        match previous_channels_by_id.get(channel_id) {
            None => detected_changes.push(Change::ChannelAdded((*channel_id).clone())),
            Some(previous_channel) => {
                if previous_channel != current_channel {
                    detected_changes.push(Change::ChannelChanged((*channel_id).clone()));
                }
            }
        }
    }
    for channel_id in previous_channels_by_id.keys() {
        if !current_channels_by_id.contains_key(*channel_id) {
            detected_changes.push(Change::ChannelRemoved((*channel_id).clone()));
        }
    }

    detected_changes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{Endpoint, HttpMethod};
    use crate::types::{ObjectType, TypeDeclaration, TypeShape};

    fn empty_endpoint(id: &str, path: &str) -> Endpoint {
        Endpoint {
            id: EndpointId(id.to_string()),
            name: id.to_string(),
            method: HttpMethod::Get,
            path: path.to_string(),
            url_resolution: Default::default(),
            server_url: None,
            accept: None,
            summary: None,
            docs: None,
            tags: vec![],
            path_parameters: vec![],
            query_parameters: vec![],
            headers: vec![],
            cookies: vec![],
            request_body: None,
            request_schema: None,
            request_body_encoding: Default::default(),
            request_media_type: None,
            form_field_serializations: Default::default(),
            multipart_field_encodings: Default::default(),
            responses: Default::default(),
            response_schemas: Default::default(),
            wildcard_error: None,
            wildcard_error_encoding: Default::default(),
            wildcard_error_media_type: None,
            auth: vec![],
            authz: None,
            pagination: None,
            streaming: None,
            conditional_streaming: None,
            cli: None,
        }
    }

    fn empty_type(id: &str) -> TypeDeclaration {
        TypeDeclaration {
            id: TypeId(id.to_string()),
            name: id.to_string(),
            docs: None,
            shape: TypeShape::Object(ObjectType {
                extends: vec![],
                fields: vec![],
                extra_properties: false,
                extra_properties_type: None,
            }),
        }
    }

    #[test]
    fn detects_added_and_removed_endpoints() {
        let old = Api {
            types: vec![],
            endpoints: vec![empty_endpoint("GET:/a", "/a")],
            cli: Default::default(),
            ..Default::default()
        };
        let new = Api {
            types: vec![],
            endpoints: vec![empty_endpoint("GET:/b", "/b")],
            cli: Default::default(),
            ..Default::default()
        };

        let changes = diff_api_snapshots(&old, &new);
        assert!(changes.contains(&Change::EndpointRemoved(EndpointId("GET:/a".into()))));
        assert!(changes.contains(&Change::EndpointAdded(EndpointId("GET:/b".into()))));
    }

    #[test]
    fn detects_changed_endpoint_by_path() {
        let old = Api {
            types: vec![],
            endpoints: vec![empty_endpoint("GET:/a", "/a")],
            cli: Default::default(),
            ..Default::default()
        };
        let mut changed = empty_endpoint("GET:/a", "/a/renamed");
        changed.id = EndpointId("GET:/a".into());
        let new = Api {
            types: vec![],
            endpoints: vec![changed],
            cli: Default::default(),
            ..Default::default()
        };

        assert_eq!(
            diff_api_snapshots(&old, &new),
            vec![Change::EndpointChanged(EndpointId("GET:/a".into()))]
        );
    }

    #[test]
    fn detects_changed_endpoint_delivery_and_url_resolution() {
        let old = Api {
            endpoints: vec![empty_endpoint("POST:/upload", "/{upload_url}")],
            ..Default::default()
        };
        let mut endpoint = empty_endpoint("POST:/upload", "/{upload_url}");
        endpoint.url_resolution = crate::http::UrlResolution::CallerSuppliedAbsolute {
            parameter_name: "uploadUrl".to_string(),
        };
        endpoint.conditional_streaming = Some(crate::http::ConditionalStreaming {
            request_body_field: "stream".to_string(),
            kind: crate::http::StreamingKind::Json,
            payload: crate::types::TypeRef::Primitive(crate::types::PrimitiveType::String),
        });
        let new = Api {
            endpoints: vec![endpoint],
            ..Default::default()
        };

        assert_eq!(
            diff_api_snapshots(&old, &new),
            vec![Change::EndpointChanged(EndpointId(
                "POST:/upload".to_string()
            ))]
        );
    }

    #[test]
    fn no_changes_for_identical_snapshots() {
        let api = Api {
            types: vec![empty_type("Pet")],
            endpoints: vec![empty_endpoint("GET:/a", "/a")],
            cli: Default::default(),
            ..Default::default()
        };
        assert!(diff_api_snapshots(&api, &api).is_empty());
    }

    #[test]
    fn detects_cli_behavior_changes() {
        let old = Api::default();
        let mut new = Api::default();
        new.cli.base_url = Some("https://api.example.test".to_string());

        assert_eq!(
            diff_api_snapshots(&old, &new),
            vec![Change::CliBehaviorChanged]
        );
    }

    #[test]
    fn detects_sdk_behavior_changes() {
        let old = Api::default();
        let mut new = Api::default();
        new.sdk.package_name = Some("@example/sdk".to_string());

        assert_eq!(
            diff_api_snapshots(&old, &new),
            vec![Change::SdkBehaviorChanged]
        );
    }

    fn empty_channel(id: &str, path: &str) -> crate::websocket::WebSocketChannel {
        crate::websocket::WebSocketChannel {
            id: ChannelId(id.to_string()),
            name: id.to_string(),
            path: path.to_string(),
            server_url: None,
            summary: None,
            docs: None,
            tags: vec![],
            path_parameters: vec![],
            query_parameters: vec![],
            auth: vec![],
            direction: crate::websocket::WebSocketDirection::ServerToClient {
                receive: crate::types::TypeRef::Primitive(crate::types::PrimitiveType::String),
            },
        }
    }

    #[test]
    fn detects_added_removed_and_changed_channels() {
        let old = Api {
            channels: vec![empty_channel("chat", "/chat/ws")],
            ..Default::default()
        };
        let mut changed = empty_channel("chat", "/chat/ws");
        changed.direction = crate::websocket::WebSocketDirection::Bidirectional {
            send: crate::types::TypeRef::Primitive(crate::types::PrimitiveType::String),
            receive: crate::types::TypeRef::Primitive(crate::types::PrimitiveType::String),
        };
        let new = Api {
            channels: vec![changed, empty_channel("notifications", "/notify/ws")],
            ..Default::default()
        };

        let changes = diff_api_snapshots(&old, &new);
        assert!(changes.contains(&Change::ChannelChanged(ChannelId("chat".into()))));
        assert!(changes.contains(&Change::ChannelAdded(ChannelId("notifications".into()))));
    }

    #[test]
    fn detects_omission_metadata_changes() {
        let old = Api::default();
        let mut new = Api::default();
        new.omissions.webhook_handlers.insert(HttpMethod::Post, 2);

        assert_eq!(
            diff_api_snapshots(&old, &new),
            vec![Change::OmissionsChanged]
        );
    }

    #[test]
    fn detects_schema_component_changes() {
        let old = Api::default();
        let mut new = Api::default();
        new.schema_components
            .insert("Pet".to_string(), serde_json::json!({ "type": "object" }));

        assert_eq!(
            diff_api_snapshots(&old, &new),
            vec![Change::SchemaComponentsChanged]
        );
    }
}
