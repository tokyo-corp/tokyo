#![allow(missing_docs)]

use tokyo_ir::http::{
    BodyEncoding, HttpMethod, QuerySerialization, ResponseEncoding, StreamingKind, UrlResolution,
};
use tokyo_ir::pagination::Pagination;
use tokyo_ir::types::{PrimitiveType, TypeRef, TypeShape};

fn document(fragment: &str) -> String {
    format!("openapi: 3.1.0\ninfo: {{ title: test, version: 1.0.0 }}\n{fragment}\n")
}

fn unsupported(fragment: &str, expected: &str) {
    let error = tokyo_import_openapi::import_openapi_yaml_document(&document(fragment))
        .expect_err("fixture should be rejected")
        .to_string();
    assert!(
        error.contains(expected),
        "expected `{expected}` in `{error}`"
    );
}

#[test]
fn rejects_cyclic_schema_references_without_crashing() {
    // A `$ref` cycle previously recursed until the process aborted on a stack
    // overflow; it must now surface as a clean, catchable import error.
    unsupported(
        "components:\n  schemas:\n    A:\n      $ref: '#/components/schemas/B'\n    B:\n      $ref: '#/components/schemas/A'\n",
        "cyclic schema reference",
    );
    unsupported(
        "components:\n  schemas:\n    A:\n      $ref: '#/components/schemas/A'\n",
        "cyclic schema reference",
    );
}

#[test]
fn imports_coverage_fixture_into_typed_behavior() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(include_str!(
        "../../../../examples/openapi-coverage.yaml"
    ))
    .expect("coverage fixture should import");
    assert_eq!(
        api.cli.base_url.as_deref(),
        Some("https://us.api.example.test/v1")
    );

    let labels = api
        .types
        .iter()
        .find(|declaration| declaration.name == "Labels")
        .unwrap();
    let TypeShape::Object(labels) = &labels.shape else {
        panic!("Labels should be an object");
    };
    assert_eq!(
        labels.extra_properties_type.as_deref(),
        Some(&TypeRef::Primitive(PrimitiveType::String))
    );
    let status = api
        .types
        .iter()
        .find(|declaration| declaration.name == "Status")
        .unwrap();
    let TypeShape::Enum(status) = &status.shape else {
        panic!("Status should be an enum");
    };
    assert!(status.forward_compatible);

    let list = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "listItems")
        .unwrap();
    assert_eq!(list.summary.as_deref(), Some("List items"));
    assert_eq!(list.tags, ["Items"]);
    assert!(matches!(list.pagination, Some(Pagination::Cursor { .. })));
    assert_eq!(
        list.query_parameters
            .iter()
            .find(|parameter| parameter.wire_name == "filter")
            .unwrap()
            .serialization,
        QuerySerialization::DeepObject
    );
    assert!(
        list.query_parameters
            .iter()
            .any(|parameter| parameter.docs.is_some())
    );

    let form = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "submitForm")
        .unwrap();
    assert_eq!(form.request_body_encoding, BodyEncoding::Form);
    let events = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "watchEvents")
        .unwrap();
    assert_eq!(
        events.streaming,
        Some(StreamingKind::Sse { resumable: false })
    );
}

#[test]
fn preserves_normalized_json_schemas_refs_and_components() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(
        r##"
openapi: 3.0.3
info: { title: test, version: 1.0.0 }
components:
  schemas:
    Envelope:
      type: object
      required: [item]
      properties:
        item: { $ref: "#/components/schemas/Item" }
    Item:
      type: string
      nullable: true
paths:
  /items:
    post:
      operationId: createItem
      requestBody:
        required: true
        content:
          application/json:
            schema: { $ref: "#/components/schemas/Envelope" }
      responses:
        "201":
          description: created
          content:
            application/json:
              schema: { $ref: "#/components/schemas/Item" }
        default:
          description: error
          content:
            application/json:
              schema: { type: object, additionalProperties: true }
"##,
    )
    .expect("schema metadata should import");

    let endpoint = &api.endpoints[0];
    assert!(endpoint.request_body.is_some(), "TypeRef remains available");
    assert_eq!(
        endpoint.request_schema.as_ref().unwrap()["$ref"],
        "#/components/schemas/Envelope"
    );
    assert_eq!(
        endpoint.response_schemas["201"]["$ref"],
        "#/components/schemas/Item"
    );
    assert_eq!(
        endpoint.response_schemas["default"]["type"],
        serde_json::json!("object")
    );
    assert_eq!(
        api.schema_components["Item"]["type"],
        serde_json::json!(["string", "null"])
    );
}

#[test]
fn imports_cli_extensions_into_typed_overrides() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /widgets:
    get:
      operationId: listWidgets
      x-tokyo-cli-name: ls
      x-tokyo-cli-aliases: [list, l]
      responses:
        "200":
          description: ok
    delete:
      operationId: deleteWidget
      x-tokyo-cli-hidden: true
      x-tokyo-cli-ignore: true
      responses:
        "204":
          description: ok
    put:
      operationId: replaceWidget
      responses:
        "204":
          description: ok
"#,
    ))
    .expect("fixture should import");

    let list = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "listWidgets")
        .unwrap();
    let overrides = list
        .cli
        .as_ref()
        .expect("listWidgets should carry cli overrides");
    assert_eq!(overrides.name.as_deref(), Some("ls"));
    assert_eq!(overrides.aliases, vec!["list".to_string(), "l".to_string()]);
    assert!(!overrides.hidden);
    assert!(!overrides.ignore);

    let delete = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "deleteWidget")
        .unwrap();
    let overrides = delete
        .cli
        .as_ref()
        .expect("deleteWidget should carry cli overrides");
    assert!(overrides.hidden);
    assert!(overrides.ignore);

    let replace = api
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == "replaceWidget")
        .unwrap();
    assert!(
        replace.cli.is_none(),
        "an operation with no x-tokyo-cli-* keys should carry no overrides"
    );
}

#[test]
fn imports_offset_pagination_and_text_streaming_extensions() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r##"
paths:
  /items:
    get:
      operationId: listItems
      x-tokyo-pagination:
        kind: offset
        offsetParam: offset
        hasNextField: hasNext
        step: 25
      parameters:
        - name: offset
          in: query
          schema: { type: integer }
      responses:
        "200":
          description: page
          content:
            application/json:
              schema:
                type: object
                properties:
                  hasNext: { type: boolean }
  /logs:
    get:
      operationId: streamLogs
      x-tokyo-streaming: text
      responses:
        "200":
          description: logs
          content:
            text/plain:
              schema: { type: string }
"##,
    ))
    .expect("extensions should import");

    assert!(matches!(
        api.endpoints[0].pagination,
        Some(Pagination::Offset { step: Some(25), .. })
    ));
    assert_eq!(api.endpoints[1].streaming, Some(StreamingKind::Text));
}

#[test]
fn infers_standard_streaming_media_types() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /events:
    get:
      operationId: streamEvents
      responses:
        "200":
          description: events
          content:
            text/event-stream:
              schema: { type: string }
  /records:
    get:
      operationId: streamRecords
      responses:
        "200":
          description: records
          content:
            application/x-ndjson:
              schema: { type: object }
"#,
    ))
    .expect("standard streaming media should infer stream behavior");

    assert!(api.endpoints.iter().any(|endpoint| matches!(
        endpoint.streaming,
        Some(StreamingKind::Sse { resumable: false })
    )));
    assert!(
        api.endpoints
            .iter()
            .any(|endpoint| endpoint.streaming == Some(StreamingKind::Json))
    );
}

#[test]
fn preserves_conditional_json_and_stream_delivery() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /completions:
    post:
      operationId: createCompletion
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              properties:
                stream: { type: boolean }
                prompt: { type: string }
      responses:
        "200":
          description: completion
          content:
            application/json:
              schema: { type: string }
            text/event-stream:
              schema: { type: integer }
"#,
    ))
    .expect("body-selected stream should import alongside JSON");

    let endpoint = &api.endpoints[0];
    assert_eq!(endpoint.streaming, None);
    assert_eq!(endpoint.responses[&200].encoding, ResponseEncoding::Json);
    let conditional = endpoint
        .conditional_streaming
        .as_ref()
        .expect("conditional stream metadata");
    assert_eq!(conditional.request_body_field, "stream");
    assert_eq!(conditional.kind, StreamingKind::Sse { resumable: false });
    assert_eq!(
        conditional.payload,
        TypeRef::Primitive(PrimitiveType::Integer)
    );
}

#[test]
fn detects_conditional_delivery_across_success_statuses() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /records:
    post:
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                stream: { type: boolean }
      responses:
        "200":
          description: buffered
          content:
            application/json:
              schema: { type: array, items: { type: string } }
        "202":
          description: streamed
          content:
            application/x-ndjson:
              schema: { type: string }
"#,
    ))
    .expect("stream alternatives split across 2xx statuses should import");

    let conditional = api.endpoints[0]
        .conditional_streaming
        .as_ref()
        .expect("conditional stream metadata");
    assert_eq!(conditional.kind, StreamingKind::Json);
    assert_eq!(
        conditional.payload,
        TypeRef::Primitive(PrimitiveType::String)
    );
}

#[test]
fn rejects_mixed_json_and_stream_without_boolean_selector() {
    unsupported(
        "paths: { /x: { get: { responses: { '200': { description: ok, content: { application/json: { schema: { type: string } }, application/x-ndjson: { schema: { type: string } } } } } } } }",
        "without a boolean request-body `stream` field",
    );
}

#[test]
fn imports_deep_object_form_field_encoding() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /forms:
    post:
      operationId: submitForm
      requestBody:
        required: true
        content:
          application/x-www-form-urlencoded:
            schema:
              type: object
              properties:
                expand:
                  type: array
                  items: { type: string }
            encoding:
              expand: { style: deepObject, explode: true }
      responses:
        "204": { description: ok }
"#,
    ))
    .expect("deepObject form fields should import");

    assert_eq!(
        api.endpoints[0].form_field_serializations["expand"].serialization,
        QuerySerialization::DeepObject
    );
}

#[test]
fn imports_single_operation_server_with_defaulted_variables() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /files:
    post:
      servers:
        - url: https://{region}.files.example.test/
          variables:
            region: { default: us }
      responses:
        "204": { description: ok }
"#,
    ))
    .expect("one operation server should import");

    assert_eq!(
        api.endpoints[0].server_url.as_deref(),
        Some("https://us.files.example.test/")
    );
}

#[test]
fn imports_caller_supplied_absolute_upload_url() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /<upload_url>:
    post:
      operationId: uploadImage
      servers:
        - url: https://
      responses:
        "204": { description: uploaded }
"#,
    ))
    .expect("scheme-only upload server should use caller URL");

    let endpoint = &api.endpoints[0];
    assert_eq!(endpoint.server_url, None);
    assert_eq!(
        endpoint.url_resolution,
        UrlResolution::CallerSuppliedAbsolute {
            parameter_name: "uploadUrl".to_string()
        }
    );
    assert_eq!(
        endpoint.path_parameters[0].r#type,
        TypeRef::Primitive(PrimitiveType::String)
    );
    assert_eq!(endpoint.path_parameters[0].wire_name, "upload_url");
}

#[test]
fn supports_path_servers_with_operation_precedence() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /files:
    servers:
      - url: https://path.example.test
    get:
      operationId: getFiles
      responses:
        "204": { description: ok }
    post:
      operationId: createFile
      servers:
        - url: https://operation.example.test
      responses:
        "204": { description: ok }
"#,
    ))
    .expect("single path server should apply unless operation overrides it");

    assert_eq!(
        api.endpoints[0].server_url.as_deref(),
        Some("https://path.example.test")
    );
    assert_eq!(
        api.endpoints[1].server_url.as_deref(),
        Some("https://operation.example.test")
    );
}

#[test]
fn rejects_ambiguous_scheme_only_servers() {
    unsupported(
        "paths: { /files: { post: { servers: [{ url: 'https://' }], responses: { '204': { description: ok } } } } }",
        "path that is not exactly one parameter",
    );
    unsupported(
        "paths: { '/{url}': { parameters: [{ name: url, in: path, required: true, schema: { type: string } }], post: { servers: [{ url: 'http://' }], responses: { '204': { description: ok } } } } }",
        "ambiguous scheme-only server `http://`",
    );
}

#[test]
fn imports_plain_text_request_bodies() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /markdown:
    post:
      requestBody:
        required: true
        content:
          text/plain:
            schema: { type: string }
          text/x-markdown:
            schema: { type: string }
      responses:
        "204": { description: ok }
"#,
    ))
    .expect("plain text should be selected from equivalent textual alternatives");

    assert_eq!(api.endpoints[0].request_body_encoding, BodyEncoding::Text);
    assert_eq!(
        api.endpoints[0].request_body,
        Some(TypeRef::Primitive(PrimitiveType::String))
    );
}

#[test]
fn selects_json_from_response_alternatives() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /resource:
    get:
      responses:
        "200":
          description: ok
          content:
            application/json:
              schema: { type: string }
            text/plain:
              schema: { type: string }
"#,
    ))
    .expect("JSON response alternatives should be selected explicitly");

    assert_eq!(api.endpoints[0].accept.as_deref(), Some("application/json"));
    assert_eq!(
        api.endpoints[0].responses[&200].encoding,
        ResponseEncoding::Json
    );
}

#[test]
fn imports_response_links_without_automatic_traversal() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /items:
    get:
      operationId: listItems
      responses:
        "200":
          description: ok
          links:
            next:
              operationId: listItems
          content:
            application/json:
              schema: { type: string }
"#,
    ))
    .expect("response links should not block direct client generation");

    assert_eq!(api.endpoints.len(), 1);
    assert_eq!(
        api.endpoints[0].responses[&200].body,
        Some(TypeRef::Primitive(PrimitiveType::String))
    );
}

#[test]
fn imports_textual_vendor_response_media() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /export:
    get:
      operationId: exportRows
      responses:
        "200":
          description: csv
          content:
            text/csv:
              schema: { type: string }
  /config:
    get:
      operationId: exportConfig
      responses:
        "200":
          description: yaml
          content:
            application/yaml:
              schema: { type: string }
"#,
    ))
    .expect("text media types should import");

    assert!(
        api.endpoints
            .iter()
            .all(|endpoint| endpoint.responses[&200].encoding == ResponseEncoding::Text)
    );
}

#[test]
fn imports_yaml_constraints_beyond_json_integer_range() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    HugeCounter:
      type: integer
      minimum: 0
      maximum: 18446744073709552000
paths: {}
"#,
    ))
    .expect("oversized numeric constraints should not block type generation");

    assert!(
        api.types
            .iter()
            .any(|declaration| declaration.name == "HugeCounter")
    );
}

#[test]
fn preserves_requiredness_independently_from_nullability() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    NullableFields:
      type: object
      required: [requiredValue]
      properties:
        requiredValue:
          type: [string, "null"]
        optionalValue:
          type: [string, "null"]
paths: {}
"#,
    ))
    .expect("nullable fields should import");
    let declaration = api
        .types
        .iter()
        .find(|declaration| declaration.name == "NullableFields")
        .expect("nullable object should be declared");
    let TypeShape::Object(object) = &declaration.shape else {
        panic!("nullable fixture should be an object");
    };
    assert_eq!(
        object.fields[0].r#type,
        TypeRef::Nullable(Box::new(TypeRef::Primitive(PrimitiveType::String)))
    );
    assert_eq!(
        object.fields[1].r#type,
        TypeRef::Optional(Box::new(TypeRef::Nullable(Box::new(TypeRef::Primitive(
            PrimitiveType::String
        )))))
    );
}

#[test]
fn collapses_any_of_null_without_empty_synthetic_variants() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    NullableFields:
      type: object
      required: [requiredValue]
      properties:
        requiredValue:
          anyOf:
            - { type: string }
            - { type: "null" }
        optionalValue:
          anyOf:
            - { type: "null" }
            - { type: integer }
paths: {}
"#,
    ))
    .expect("anyOf nullable fields should import");
    let declaration = api
        .types
        .iter()
        .find(|declaration| declaration.name == "NullableFields")
        .expect("nullable object should be declared");
    let TypeShape::Object(object) = &declaration.shape else {
        panic!("nullable fixture should be an object");
    };
    assert_eq!(
        object.fields[0].r#type,
        TypeRef::Nullable(Box::new(TypeRef::Primitive(PrimitiveType::String)))
    );
    assert_eq!(
        object.fields[1].r#type,
        TypeRef::Optional(Box::new(TypeRef::Nullable(Box::new(TypeRef::Primitive(
            PrimitiveType::Integer
        )))))
    );
    assert!(
        api.types
            .iter()
            .all(|declaration| !declaration.name.contains("Variant"))
    );
}

#[test]
fn treats_unconstrained_open_string_union_members_as_strings() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    OpenAction:
      anyOf:
        - { type: string, enum: [pick_up] }
        - { type: string, enum: [drop_off] }
        - {}
paths: {}
"#,
    ))
    .expect("open string union should import");
    let declaration = api
        .types
        .iter()
        .find(|declaration| declaration.name == "OpenAction")
        .expect("open action should be declared");
    let TypeShape::UndiscriminatedUnion { variants } = &declaration.shape else {
        panic!("open action should remain an undiscriminated union");
    };
    assert_eq!(
        variants.last(),
        Some(&TypeRef::Primitive(PrimitiveType::String))
    );
    assert!(
        api.types
            .iter()
            .all(|declaration| declaration.name != "OpenActionVariant2")
    );
}

#[test]
fn operation_parameters_override_path_item_parameters_in_place() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /items:
    parameters:
      - { name: shared, in: query, schema: { type: string } }
      - { name: inherited, in: query, schema: { type: boolean } }
    get:
      operationId: listItems
      parameters:
        - { name: shared, in: query, required: true, schema: { type: integer } }
        - { name: added, in: query, schema: { type: string } }
      responses:
        "204": { description: ok }
"#,
    ))
    .expect("parameter overrides should import");
    let parameters = &api.endpoints[0].query_parameters;
    assert_eq!(
        parameters
            .iter()
            .map(|parameter| parameter.wire_name.as_str())
            .collect::<Vec<_>>(),
        ["shared", "inherited", "added"]
    );
    assert_eq!(
        parameters[0].r#type,
        TypeRef::Primitive(PrimitiveType::Integer)
    );
}

#[test]
fn rejects_patterned_response_statuses_without_collapsing_default() {
    unsupported(
        "paths: { /x: { get: { responses: { default: { description: fallback }, '4XX': { description: range } } } } }",
        "get /x response status pattern `4XX`",
    );
}

#[test]
fn rejects_normalized_name_collisions_contextually() {
    unsupported(
        "paths: { /a: { get: { operationId: foo-bar, responses: { '204': { description: ok } } } }, /b: { get: { operationId: foo_bar, responses: { '204': { description: ok } } } } }",
        "endpoint method name collision",
    );
    unsupported(
        "paths: { '/x/{foo-bar}': { get: { operationId: getX, parameters: [{ name: foo-bar, in: path, required: true, schema: { type: string } }, { name: foo_bar, in: query, schema: { type: string } }], responses: { '204': { description: ok } } } } }",
        "parameter object name collision",
    );
    unsupported(
        "paths: { /x: { post: { operationId: postX, parameters: [{ name: body, in: query, schema: { type: string } }], requestBody: { content: { application/json: { schema: { type: string } } } }, responses: { '204': { description: ok } } } } }",
        "conflicts with request body property `body`",
    );
}

#[test]
fn disambiguates_component_schemas_that_normalize_to_the_same_type_name() {
    // Stripe declares both `billing.alert.triggered` and `billing.alert_triggered`;
    // colliding component names must get deterministic suffixed identifiers, and
    // `$ref`s must resolve to the disambiguated ids rather than the raw normalization.
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        "components: { schemas: { foo-bar: { type: object, properties: { a: { type: string } } }, foo_bar: { type: object, properties: { b: { type: string } } }, holder: { type: object, properties: { second: { $ref: '#/components/schemas/foo_bar' } } } } }\npaths: {}",
    ))
    .expect("colliding component names should disambiguate instead of failing");

    let names: Vec<&str> = api
        .types
        .iter()
        .map(|declaration| declaration.name.as_str())
        .collect();
    assert!(names.contains(&"FooBar"), "{names:?}");
    assert!(names.contains(&"FooBar2"), "{names:?}");

    let holder = api
        .types
        .iter()
        .find(|declaration| declaration.name == "Holder")
        .expect("holder should be declared");
    let TypeShape::Object(object) = &holder.shape else {
        panic!("holder should be an object");
    };
    let second = object
        .fields
        .iter()
        .find(|field| field.wire_name == "second")
        .expect("holder.second should be declared");
    let TypeRef::Optional(inner) = &second.r#type else {
        panic!("optional field expected, got {:?}", second.r#type);
    };
    assert_eq!(
        **inner,
        TypeRef::Named(tokyo_ir::id::TypeId("FooBar2".to_string())),
        "$ref must follow the disambiguated id for the later declaration"
    );
}

#[test]
fn preserves_wire_fields_that_share_an_unused_normalized_name() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        "components: { schemas: { Reactions: { type: object, properties: { '+1': { type: integer }, '-1': { type: integer } } } } }\npaths: {}",
    ))
    .expect("exact wire field names do not occupy generated identifier symbols");
    let declaration = api
        .types
        .iter()
        .find(|declaration| declaration.name == "Reactions")
        .expect("Reactions should be declared");
    let TypeShape::Object(object) = &declaration.shape else {
        panic!("Reactions should be an object");
    };
    assert_eq!(
        object
            .fields
            .iter()
            .map(|field| field.wire_name.as_str())
            .collect::<Vec<_>>(),
        ["+1", "-1"]
    );
}

#[test]
fn intersects_repeated_all_of_fields_without_duplicate_declarations() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    Model:
      allOf:
        - type: object
          required: [status]
          properties:
            status: { type: string }
        - type: object
          properties:
            status: { type: string, enum: [ready] }
paths: {}
"#,
    ))
    .expect("repeated allOf fields should merge");
    let declaration = api
        .types
        .iter()
        .find(|declaration| declaration.name == "Model")
        .expect("Model should be declared");
    let TypeShape::Object(object) = &declaration.shape else {
        panic!("Model should be an object");
    };
    assert_eq!(object.fields.len(), 1);
    assert_eq!(object.fields[0].wire_name, "status");
    assert!(!matches!(object.fields[0].r#type, TypeRef::Optional(_)));
}

#[test]
fn allocates_inline_type_ids_around_reserved_components() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    AccountBusinessProfile:
      type: string
    Account:
      type: object
      properties:
        business_profile:
          type: object
          properties:
            support_phone: { type: string }
paths: {}
"#,
    ))
    .expect("inline/component collisions should receive distinct IDs");

    assert!(
        api.types
            .iter()
            .any(|declaration| declaration.name == "AccountBusinessProfile")
    );
    assert!(
        api.types
            .iter()
            .any(|declaration| declaration.name == "AccountBusinessProfile2")
    );
    let account = api
        .types
        .iter()
        .find(|declaration| declaration.name == "Account")
        .expect("Account should be declared");
    let TypeShape::Object(account) = &account.shape else {
        panic!("Account should be an object");
    };
    assert_eq!(
        account.fields[0].r#type,
        TypeRef::Optional(Box::new(TypeRef::Named(tokyo_ir::id::TypeId(
            "AccountBusinessProfile2".to_string()
        ))))
    );
}

#[test]
fn imports_fixed_and_rest_prefix_item_tuples() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
components:
  schemas:
    FixedTuple:
      type: array
      prefixItems: [{ type: string }, { type: [integer, "null"] }]
      minItems: 2
      maxItems: 2
    RestTuple:
      type: array
      prefixItems: [{ type: string }]
      minItems: 1
      items: { type: boolean }
paths: {}
"#,
    ))
    .expect("prefixItems tuples should import");

    assert_eq!(
        api.types[0].shape,
        TypeShape::Alias {
            target: TypeRef::Tuple {
                items: vec![
                    TypeRef::Primitive(PrimitiveType::String),
                    TypeRef::Nullable(Box::new(TypeRef::Primitive(PrimitiveType::Integer))),
                ],
                rest: None,
            }
        }
    );
    assert_eq!(
        api.types[1].shape,
        TypeShape::Alias {
            target: TypeRef::Tuple {
                items: vec![TypeRef::Primitive(PrimitiveType::String)],
                rest: Some(Box::new(TypeRef::Primitive(PrimitiveType::Boolean))),
            }
        }
    );

    unsupported(
        "components: { schemas: { BoundedRest: { type: array, prefixItems: [{ type: string }], maxItems: 3 } } }\npaths: {}",
        "finitely bounded additional items",
    );
}

#[test]
fn records_root_webhooks_without_generating_client_endpoints() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r##"
components:
  schemas:
    EventPayload:
      type: object
      required: [id]
      properties:
        id: { type: string }
paths: {}
webhooks:
  event:
    post:
      requestBody:
        content:
          application/json:
            schema: { $ref: "#/components/schemas/EventPayload" }
      responses:
        "204": { description: accepted }
"##,
    ))
    .expect("root webhooks should be recorded as omissions");

    assert!(api.endpoints.is_empty());
    assert!(
        api.types
            .iter()
            .any(|declaration| declaration.name == "EventPayload")
    );
    assert_eq!(api.omissions.webhook_handler_count(), 1);
    assert_eq!(api.omissions.webhook_handlers[&HttpMethod::Post], 1);
}

#[test]
fn imports_head_options_and_trace_with_head_responses_forced_empty() {
    let api = tokyo_import_openapi::import_openapi_yaml_document(&document(
        r#"
paths:
  /probe:
    head:
      operationId: probeHead
      responses:
        "200":
          description: metadata
          content:
            application/json:
              schema: { type: string }
    options:
      operationId: probeOptions
      responses:
        "204": { description: options }
    trace:
      operationId: probeTrace
      responses:
        "200":
          description: trace
          content:
            text/plain:
              schema: { type: string }
"#,
    ))
    .expect("all standard HTTP methods should import");

    assert_eq!(api.endpoints[0].method, HttpMethod::Head);
    assert!(api.endpoints[0].responses[&200].body.is_none());
    assert_eq!(api.endpoints[1].method, HttpMethod::Options);
    assert_eq!(api.endpoints[2].method, HttpMethod::Trace);
}

#[test]
fn rejects_unsupported_openapi_constructs_contextually() {
    unsupported(
        "paths: { /x: { get: { servers: [{ url: https://one.example.test }, { url: https://two.example.test }], responses: { '204': { description: ok } } } } }",
        "multiple operation servers",
    );
    unsupported(
        "paths: { /x: { get: { parameters: [{ name: q, in: query, schema: { type: string }, content: { application/json: { schema: { type: string } } } }], responses: { '204': { description: ok } } } } }",
        "declares both schema and content",
    );
    unsupported(
        "paths: { /x: { get: { parameters: [{ name: q, in: query, content: { application/json: { schema: { type: string } }, text/plain: { schema: { type: string } } } }], responses: { '204': { description: ok } } } } }",
        "declares multiple content representations",
    );
    unsupported(
        "paths: { /x: { get: { callbacks: { changed: { '{$request.body#/url}': { post: { responses: { '204': { description: ok } } } } } }, responses: { '204': { description: ok } } } } }",
        "declares callbacks",
    );
    unsupported(
        "paths: { /x: { get: { responses: { '200': { description: ok, content: { application/json: { schema: { '$ref': 'https://example.test/schema.json' } } } } } } } }",
        "external schema reference",
    );
    unsupported(
        "paths: { /x: { get: { x-tokyo-streaming: { kind: sse, resumable: true }, responses: { '200': { description: ok, content: { text/event-stream: { schema: { type: string } } } } } } } }",
        "resumable SSE",
    );
    unsupported(
        "paths: { /x: { get: { x-tokyo-pagination: { kind: custom }, responses: { '204': { description: ok } } } } }",
        "pagination kind `custom`",
    );
    unsupported(
        "paths: { /x: { get: { parameters: [{ name: q, in: query }], responses: { '204': { description: ok } } } } }",
        "has neither schema nor content",
    );
}
