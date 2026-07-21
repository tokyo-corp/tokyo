---
name: tokyo-framework-openapi-import
description: Guides Tokyo OpenAPI 3.0/3.1 normalization, import into the shared IR, compatibility decisions, and fixture coverage. Applies when changing schemas, operations, security, serialization, streaming, pagination, extensions, importer errors, or the serialized IR contract.
---
# Tokyo framework OpenAPI import

## Follow the import boundary

- `crates/codegen/import-openapi/src/lib.rs` parses JSON/YAML, preserves raw-source distinctions, normalizes 3.0 input, builds `tokyo_ir::Api`, and validates invariants.
- `normalize.rs` contains best-effort OpenAPI 3.0-to-3.1 rewrites; `schema.rs`, `operation.rs`, `security.rs`, and `naming.rs` map supported semantics into IR.
- `crates/codegen/ir/src/` is the neutral, serializable contract shared by importers, emitters, and the engine. Keep CLI-only additions in `CliBehavior` or endpoint CLI metadata so other emitters may ignore them.
- `crates/codegen/codegen-engine/src/lib.rs` applies configuration and canonicalizes imported IR; do not duplicate frontend filesystem policy there.

## Preserve compatibility

- Match the supported and deliberately rejected behavior encoded in `crates/codegen/import-openapi/tests/coverage.rs`. Unsupported constructs must fail contextually, not disappear silently.
- Preserve requiredness separately from nullability, wire names separately from Rust names, local `$ref` identity, normalized component schemas, auth alternatives, server precedence, media types, parameter encodings, and omission metadata.
- Canonicalize collections whose source order has no meaning and validate every produced `Api`.
- For additive serialized fields, use appropriate serde defaults when old snapshots remain meaningful.
- Increment `tokyo_ir::api::IR_SCHEMA_VERSION` only when existing serialized IR cannot retain its meaning or shape compatibility. Update schema-version tests and readers together.

## Fixtures and evidence

1. Add the narrowest importer test in `crates/codegen/import-openapi/tests/coverage.rs` for mapping and rejection behavior.
2. Update a focused `examples/*.yaml` fixture when behavior must flow through emission or executable generated CLIs.
3. If a fixture changes generated output, update the matching directory under `crates/codegen/emit-cli/tests/golden/` through the golden workflow, never by selective hand edits.

## Focused validation

```sh
cargo test -p tokyo-import-openapi --test coverage <test-name> --locked
cargo test -p tokyo-import-openapi --lib <test-name> --locked
cargo test -p tokyo-ir <test-name> --locked
cargo test -p tokyo-codegen-engine <test-name> --locked
```

Run emitter golden or compile validation only when the changed IR is consumed there. Do not run broad workspace tests.
