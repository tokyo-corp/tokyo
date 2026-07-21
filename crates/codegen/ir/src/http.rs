use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::auth::AuthRequirement;
use crate::id::EndpointId;
use crate::pagination::Pagination;
use crate::types::TypeRef;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
/// HTTP method for a callable endpoint.
pub enum HttpMethod {
    /// GET request.
    Get,
    /// HEAD request.
    Head,
    /// POST request.
    Post,
    /// PUT request.
    Put,
    /// PATCH request.
    Patch,
    /// DELETE request.
    Delete,
    /// OPTIONS request.
    Options,
    /// TRACE request.
    Trace,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Callable HTTP operation lowered from OpenAPI.
pub struct Endpoint {
    /// Stable endpoint identifier.
    pub id: EndpointId,
    /// Rust-safe operation name.
    pub name: String,
    /// HTTP method.
    pub method: HttpMethod,
    /// OpenAPI path template.
    pub path: String,
    /// How the final request URL is selected.
    #[serde(default)]
    pub url_resolution: UrlResolution,
    /// Operation-level server URL, after substituting declared defaults.
    #[serde(default)]
    pub server_url: Option<String>,
    /// Response representation selected by the generated method.
    #[serde(default)]
    pub accept: Option<String>,
    /// Short operation summary.
    #[serde(default)]
    pub summary: Option<String>,
    /// Longer operation documentation.
    pub docs: Option<String>,
    /// OpenAPI tags used for command grouping.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Path template parameters.
    pub path_parameters: Vec<Parameter>,
    /// Query parameters.
    pub query_parameters: Vec<Parameter>,
    /// Header parameters.
    pub headers: Vec<Parameter>,
    /// User-supplied Cookie parameters, distinct from cookie-based security
    /// schemes in `auth`.
    #[serde(default)]
    pub cookies: Vec<Parameter>,
    /// Request body type.
    pub request_body: Option<TypeRef>,
    /// Normalized JSON Schema selected for the JSON request representation.
    /// Kept alongside `request_body`, whose `TypeRef` remains the contract used
    /// by code emitters.
    #[serde(default)]
    pub request_schema: Option<serde_json::Value>,
    #[serde(default)]
    /// Request body encoding strategy.
    pub request_body_encoding: BodyEncoding,
    /// Exact selected request media type, including vendor `+json` and custom
    /// text types. `request_body_encoding` describes how to process its bytes.
    #[serde(default)]
    pub request_media_type: Option<String>,
    /// Per-property serialization overrides for URL-encoded form bodies.
    /// Properties absent from this map use OpenAPI's form/explode defaults.
    #[serde(default)]
    pub form_field_serializations: BTreeMap<String, FormFieldEncoding>,
    /// Per-property multipart content metadata.
    #[serde(default)]
    pub multipart_field_encodings: BTreeMap<String, MultipartFieldEncoding>,
    /// Responses keyed by status code.
    #[serde(default)]
    pub responses: BTreeMap<u16, Response>,
    /// Normalized JSON response schemas keyed by their original OpenAPI status
    /// key (`"200"`, `"default"`, ...). Schema `$ref`s remain unresolved.
    #[serde(default)]
    pub response_schemas: BTreeMap<String, serde_json::Value>,
    /// Wildcard error response body type.
    #[serde(default)]
    pub wildcard_error: Option<TypeRef>,
    /// Wildcard error response decoding strategy.
    #[serde(default = "default_json_response_encoding")]
    pub wildcard_error_encoding: ResponseEncoding,
    /// Wildcard error response media type.
    #[serde(default)]
    pub wildcard_error_media_type: Option<String>,
    /// Ordered OpenAPI security alternatives (OR). Every scheme within one
    /// requirement is required (AND); an empty requirement allows anonymous
    /// access. An empty outer list means the operation is public.
    #[serde(default)]
    pub auth: Vec<AuthRequirement>,
    /// API-specific authorization policy copied from the operation's
    /// `x-authz` extension. The CLI schema and orientation view expose this
    /// data without interpreting it during request execution.
    #[serde(default)]
    pub authz: Option<serde_json::Value>,
    /// Pagination policy, when this endpoint represents one page of a collection.
    pub pagination: Option<Pagination>,
    /// Streaming response kind, when this endpoint produces a stream.
    pub streaming: Option<StreamingKind>,
    /// A stream representation selected by a boolean request-body field while
    /// the ordinary response remains JSON.
    #[serde(default)]
    pub conditional_streaming: Option<ConditionalStreaming>,
    /// CLI-specific overrides from `x-tokyo-cli-*` extensions. Additive
    /// and emitter-agnostic like [`crate::pagination::Pagination`] — other
    /// emitters that do not expose command policy may ignore it.
    #[serde(default)]
    pub cli: Option<CliOverrides>,
}

/// Per-operation CLI generation overrides, populated from `x-tokyo-cli-*`
/// OpenAPI extensions. Mirrors `x-cli-*` in tools like restish: a spec-embedded
/// escape hatch for operation naming/visibility a generator can't infer well
/// on its own.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CliOverrides {
    /// Overrides the derived command name (`x-tokyo-cli-name`).
    pub name: Option<String>,
    /// Generates the command but excludes it from `--help`/introspection
    /// listings; still invocable (`x-tokyo-cli-hidden`).
    pub hidden: bool,
    /// Skips generating a dedicated command entirely; still reachable via the
    /// `api` escape hatch (`x-tokyo-cli-ignore`).
    pub ignore: bool,
    /// Additional command name aliases (`x-tokyo-cli-aliases`).
    pub aliases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
/// Response body and decoding metadata for one HTTP status.
pub struct Response {
    /// Typed response body, or `None` for an empty response.
    pub body: Option<TypeRef>,
    /// Response decoding strategy.
    pub encoding: ResponseEncoding,
    /// Exact selected response media type. `encoding` remains the coarse
    /// decoding strategy and is intentionally not inferred by emitters.
    #[serde(default)]
    pub media_type: Option<String>,
}

impl Response {
    /// Creates metadata for an empty response.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            body: None,
            encoding: ResponseEncoding::Empty,
            media_type: None,
        }
    }

    /// Creates metadata for a typed response using the given encoding.
    #[must_use]
    pub fn new(body: TypeRef, encoding: ResponseEncoding) -> Self {
        Self {
            body: Some(body),
            encoding,
            media_type: None,
        }
    }

    /// Creates metadata for a typed response with an exact media type.
    #[must_use]
    pub fn with_media_type(
        body: TypeRef,
        encoding: ResponseEncoding,
        media_type: impl Into<String>,
    ) -> Self {
        Self {
            body: Some(body),
            encoding,
            media_type: Some(media_type.into()),
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ResponseWire {
    Current(CurrentResponseWire),
    Legacy(TypeRef),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentResponseWire {
    #[serde(default)]
    body: Option<TypeRef>,
    #[serde(default)]
    encoding: Option<ResponseEncoding>,
    #[serde(default)]
    media_type: Option<String>,
}

impl<'de> Deserialize<'de> for Response {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match ResponseWire::deserialize(deserializer)? {
            ResponseWire::Current(CurrentResponseWire {
                body,
                encoding,
                media_type,
            }) => Ok(Self {
                encoding: encoding.unwrap_or(if body.is_some() {
                    ResponseEncoding::Json
                } else {
                    ResponseEncoding::Empty
                }),
                body,
                media_type,
            }),
            ResponseWire::Legacy(body) => Ok(Self::new(body, ResponseEncoding::Json)),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Coarse response decoding strategy.
pub enum ResponseEncoding {
    /// No response body is expected.
    #[default]
    Empty,
    /// Decode as JSON.
    Json,
    /// Decode as UTF-8 text.
    Text,
    /// Preserve raw bytes.
    Binary,
}

fn default_json_response_encoding() -> ResponseEncoding {
    ResponseEncoding::Json
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Operation parameter metadata.
pub struct Parameter {
    /// Wire-format parameter name.
    pub wire_name: String,
    /// Rust-safe parameter name.
    pub name: String,
    /// Parameter type.
    pub r#type: TypeRef,
    /// Optional human-facing documentation.
    #[serde(default)]
    pub docs: Option<String>,
    /// Query/form/path/header serialization style.
    #[serde(default)]
    pub serialization: QuerySerialization,
    /// Preserve reserved RFC 3986 characters in a query value. OpenAPI only
    /// defines this for query parameters and URL-encoded form fields.
    #[serde(default)]
    pub allow_reserved: bool,
    /// An OpenAPI-declared example value, if any. Additive metadata: emitters
    /// may use it (e.g. as a generated CLI flag's default) or ignore it.
    #[serde(default)]
    pub example: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
/// Strategy for resolving the final request URL.
pub enum UrlResolution {
    /// Resolve the endpoint path against its operation or SDK base URL.
    #[default]
    BaseUrlAndPath,
    /// Use the named path parameter as the complete absolute request URL.
    CallerSuppliedAbsolute {
        /// Path parameter whose value is the absolute request URL.
        parameter_name: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Streaming response selected by a request-body field.
pub struct ConditionalStreaming {
    /// Wire name of the boolean JSON request-body property selecting streaming.
    pub request_body_field: String,
    /// Stream format emitted when the selector is true.
    pub kind: StreamingKind,
    /// Item type carried by the stream representation.
    pub payload: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Kind of streaming response.
pub enum StreamingKind {
    /// JSON item stream.
    Json,
    /// UTF-8 text stream.
    Text,
    /// Server-sent event stream.
    Sse {
        /// Whether the stream supports resumable events.
        resumable: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
/// Request body wire encoding.
pub enum BodyEncoding {
    /// JSON request body.
    #[default]
    Json,
    /// UTF-8 text request body.
    Text,
    /// Raw binary request body.
    Binary,
    /// Multipart form request body.
    Multipart,
    /// URL-encoded form request body.
    Form,
}

/// How an array-valued query parameter should be serialized. Only meaningful for
/// query params — path/header params ignore this. `Form { explode: true }` is
/// OpenAPI's own default (repeated `?x=1&x=2`), which is why it's also ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuerySerialization {
    /// Form-style query serialization.
    Form {
        /// Whether array/object values are exploded into repeated parameters.
        explode: bool,
    },
    /// Space-delimited array serialization.
    SpaceDelimited,
    /// Pipe-delimited array serialization.
    PipeDelimited,
    /// Deep-object query serialization.
    DeepObject,
    /// Simple serialization.
    Simple {
        /// Whether array/object values are exploded.
        explode: bool,
    },
    /// Label serialization.
    Label {
        /// Whether array/object values are exploded.
        explode: bool,
    },
    /// Matrix serialization.
    Matrix {
        /// Whether array/object values are exploded.
        explode: bool,
    },
}

impl Default for QuerySerialization {
    fn default() -> Self {
        QuerySerialization::Form { explode: true }
    }
}

/// Complete URL-encoded form-field wire metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormFieldEncoding {
    /// Serialization style for the field.
    pub serialization: QuerySerialization,
    /// Whether reserved URI characters should remain unescaped.
    #[serde(default)]
    pub allow_reserved: bool,
}

/// A caller-supplied header attached to one multipart part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultipartHeader {
    /// Wire-format header name.
    pub wire_name: String,
    /// Header value type.
    pub r#type: TypeRef,
    /// Optional human-facing documentation.
    #[serde(default)]
    pub docs: Option<String>,
}

/// Exact metadata for one multipart property.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MultipartFieldEncoding {
    /// Optional part content type.
    #[serde(default)]
    pub content_type: Option<String>,
    /// Per-part headers.
    #[serde(default)]
    pub headers: Vec<MultipartHeader>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::EndpointId;
    use crate::types::PrimitiveType;

    #[test]
    fn response_deserializes_legacy_type_ref_as_json() {
        let response: Response =
            serde_json::from_str(r#"{"Primitive":"String"}"#).expect("legacy response should load");

        assert_eq!(
            response,
            Response::new(
                TypeRef::Primitive(PrimitiveType::String),
                ResponseEncoding::Json
            )
        );
    }

    #[test]
    fn response_defaults_encoding_from_body_presence() {
        let typed: Response = serde_json::from_str(r#"{"body":{"Primitive":"String"}}"#)
            .expect("typed response should load");
        let empty: Response =
            serde_json::from_str(r#"{"body":null}"#).expect("empty response should load");

        assert_eq!(typed.encoding, ResponseEncoding::Json);
        assert_eq!(empty.encoding, ResponseEncoding::Empty);
    }

    #[test]
    fn endpoint_defaults_additive_delivery_and_url_resolution_fields() {
        let endpoint = Endpoint {
            id: EndpointId("GET:/items".to_string()),
            name: "getItems".to_string(),
            method: HttpMethod::Get,
            path: "/items".to_string(),
            url_resolution: UrlResolution::BaseUrlAndPath,
            server_url: None,
            accept: None,
            summary: None,
            docs: None,
            tags: Vec::new(),
            path_parameters: Vec::new(),
            query_parameters: Vec::new(),
            headers: Vec::new(),
            cookies: Vec::new(),
            request_body: None,
            request_schema: None,
            request_body_encoding: BodyEncoding::Json,
            request_media_type: None,
            form_field_serializations: BTreeMap::new(),
            multipart_field_encodings: BTreeMap::new(),
            responses: BTreeMap::new(),
            response_schemas: BTreeMap::new(),
            wildcard_error: None,
            wildcard_error_encoding: ResponseEncoding::Json,
            wildcard_error_media_type: None,
            auth: Vec::new(),
            authz: None,
            pagination: None,
            streaming: None,
            conditional_streaming: None,
            cli: None,
        };
        let mut value = serde_json::to_value(endpoint).expect("serialize endpoint");
        let object = value.as_object_mut().expect("endpoint should be an object");
        object.remove("url_resolution");
        object.remove("conditional_streaming");
        object.remove("cli");
        object.remove("request_schema");
        object.remove("response_schemas");
        object.remove("authz");

        let decoded: Endpoint = serde_json::from_value(value).expect("deserialize legacy endpoint");
        assert_eq!(decoded.url_resolution, UrlResolution::BaseUrlAndPath);
        assert_eq!(decoded.conditional_streaming, None);
        assert_eq!(decoded.request_schema, None);
        assert!(decoded.response_schemas.is_empty());
        assert_eq!(decoded.authz, None);
    }
}
