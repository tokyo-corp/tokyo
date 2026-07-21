use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::cli_behavior::CliBehavior;
use crate::http::{Endpoint, HttpMethod};
use crate::sdk_behavior::SdkBehavior;
use crate::types::{TypeDeclaration, TypeShape};
use crate::websocket::WebSocketChannel;

/// Version of the serialized [`Api`] contract emitted by this release.
///
/// Increment this only for an incompatible change to the serialized IR shape.
pub const IR_SCHEMA_VERSION: u32 = 6;

const fn legacy_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Complete API model consumed by emitters and persisted in snapshots.
pub struct Api {
    /// Serialized IR schema version, distinct from the containing snapshot format.
    ///
    /// The serde default keeps snapshots written before this field existed
    /// readable as version 1.
    #[serde(default = "legacy_schema_version")]
    pub schema_version: u32,
    /// Declared reusable types.
    pub types: Vec<TypeDeclaration>,
    /// Callable HTTP operations.
    pub endpoints: Vec<Endpoint>,
    /// Persistent WebSocket channels, distinct from request/response endpoints.
    #[serde(default)]
    pub channels: Vec<WebSocketChannel>,
    /// Constructs intentionally excluded from callable client operations.
    ///
    /// Defaults preserve compatibility with IR snapshots written before
    /// omission reporting was introduced.
    #[serde(default)]
    pub omissions: OmissionMetadata,
    /// CLI-specific behavior and generated-command policy.
    #[serde(default)]
    pub cli: CliBehavior,
    /// SDK-specific behavior for generated client libraries.
    #[serde(default)]
    pub sdk: SdkBehavior,
    /// Normalized OpenAPI component schemas, retained as JSON Schema so
    /// schema-aware consumers can follow unresolved `$ref`s without
    /// reconstructing constraints from the emitter-oriented type IR.
    #[serde(default)]
    pub schema_components: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Counts of OpenAPI constructs intentionally omitted from generated clients.
pub struct OmissionMetadata {
    /// Inbound root webhook operations, grouped by HTTP method.
    #[serde(default)]
    pub webhook_handlers: BTreeMap<HttpMethod, u32>,
}

impl OmissionMetadata {
    /// Returns the total number of omitted webhook handlers.
    #[must_use]
    pub fn webhook_handler_count(&self) -> u32 {
        self.webhook_handlers.values().sum()
    }
}

impl Default for Api {
    fn default() -> Self {
        Self {
            schema_version: IR_SCHEMA_VERSION,
            types: Vec::new(),
            endpoints: Vec::new(),
            channels: Vec::new(),
            omissions: OmissionMetadata::default(),
            cli: CliBehavior::default(),
            sdk: SdkBehavior::default(),
            schema_components: BTreeMap::new(),
        }
    }
}

impl Api {
    /// Returns whether this library can safely interpret the serialized shape.
    #[must_use]
    pub const fn has_supported_schema_version(&self) -> bool {
        self.schema_version == IR_SCHEMA_VERSION
    }

    /// Normalizes collections whose source ordering has no API meaning.
    ///
    /// Emitters and persisted snapshots can call this once to get byte-stable
    /// output even when an OpenAPI parser or map implementation changes order.
    pub fn canonicalize(&mut self) {
        self.types.sort_by(|left, right| left.id.cmp(&right.id));
        for declaration in &mut self.types {
            match &mut declaration.shape {
                TypeShape::Object(object) => {
                    object.extends.sort();
                    object
                        .fields
                        .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
                }
                TypeShape::Enum(enumeration) => enumeration.values.sort_by(|left, right| {
                    left.wire_value
                        .cmp(&right.wire_value)
                        .then_with(|| left.name.cmp(&right.name))
                }),
                TypeShape::Union(union) => union.variants.sort_by(|left, right| {
                    left.discriminant_value
                        .cmp(&right.discriminant_value)
                        .then_with(|| left.variant_name.cmp(&right.variant_name))
                }),
                TypeShape::UndiscriminatedUnion { variants } => {
                    variants.sort_by_key(|variant| format!("{variant:?}"));
                }
                TypeShape::Alias { .. } => {}
            }
        }

        self.endpoints.sort_by(|left, right| left.id.cmp(&right.id));
        for endpoint in &mut self.endpoints {
            endpoint.tags.sort();
            endpoint.tags.dedup();
            if let Some(cli) = &mut endpoint.cli {
                cli.aliases.sort();
                cli.aliases.dedup();
            }
            endpoint
                .path_parameters
                .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            endpoint
                .query_parameters
                .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            endpoint
                .headers
                .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            endpoint
                .cookies
                .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            for encoding in endpoint.multipart_field_encodings.values_mut() {
                encoding
                    .headers
                    .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            }
            for alternative in &mut endpoint.auth {
                alternative
                    .schemes
                    .sort_by(|left, right| left.scheme.name.cmp(&right.scheme.name));
                for requirement in &mut alternative.schemes {
                    requirement.scopes.sort();
                }
            }
            endpoint
                .auth
                .sort_by_key(|requirement| format!("{requirement:?}"));
        }

        self.channels.sort_by(|left, right| left.id.cmp(&right.id));
        for channel in &mut self.channels {
            channel.tags.sort();
            channel.tags.dedup();
            channel
                .path_parameters
                .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            channel
                .query_parameters
                .sort_by(|left, right| left.wire_name.cmp(&right.wire_name));
            for alternative in &mut channel.auth {
                alternative
                    .schemes
                    .sort_by(|left, right| left.scheme.name.cmp(&right.scheme.name));
                for requirement in &mut alternative.schemes {
                    requirement.scopes.sort();
                }
            }
            channel
                .auth
                .sort_by_key(|requirement| format!("{requirement:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_the_current_schema_version() {
        let value = serde_json::to_value(Api::default()).expect("serialize API");
        assert_eq!(value["schema_version"], json!(IR_SCHEMA_VERSION));
    }

    #[test]
    fn treats_legacy_unversioned_ir_as_version_one() {
        let api: Api = serde_json::from_value(json!({
            "types": [],
            "endpoints": [],
            "sdk": {}
        }))
        .expect("deserialize legacy API");

        assert_eq!(api.schema_version, 1);
        assert!(!api.has_supported_schema_version());
        assert!(api.schema_components.is_empty());
    }

    #[test]
    fn preserves_unknown_versions_for_boundary_validation() {
        let api: Api = serde_json::from_value(json!({
            "schema_version": 99,
            "types": [],
            "endpoints": []
        }))
        .expect("deserialize structurally readable future API");

        assert_eq!(api.schema_version, 99);
        assert!(!api.has_supported_schema_version());
    }
}
