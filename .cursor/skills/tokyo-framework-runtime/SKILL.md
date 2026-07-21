---
name: tokyo-framework-runtime
description: Guides changes to Tokyo's generated-CLI runtime contracts, route API, transport, authentication, profiles, output, schemas, scenarios, and sessions. Applies when modifying tokyo-cli-runtime public behavior or any emitter-generated call into the runtime.
---
# Tokyo framework runtime

## Protect public contracts

- `crates/cli-runtime/src/lib.rs` defines the crate surface and handwritten-route prelude. Generated crates depend on these public names and signatures.
- `route.rs` is the user-facing route contract: `Route`, `RouteSpec`, `Argument`, `RouteContext`, middleware, request builders, responses, and run errors.
- `client.rs`, `body.rs`, and `output.rs` own HTTP wire serialization and deterministic text/JSON/binary/stream rendering.
- `config.rs`, `profile.rs`, and `oauth.rs` own generated runtime configuration, environments, credentials, login, token lifecycle, and diagnostics.
- `schema.rs`, `achieve.rs`, `scenario.rs`, and `session.rs` support agent discovery, goal execution, scenario programs, and cross-command state.
- `error.rs` defines stable exit categories, retryability, recovery hints, and machine-readable error envelopes.

## Trace cross-crate effects

1. Search `crates/codegen/emit-cli/src/` for every changed public runtime symbol before changing it.
2. Update generated templates or command emission in the same change when runtime signatures or semantics move.
3. Check `apps/codegen-cli/src/main.rs` and scaffolded dependency/version wiring when generated projects need new runtime features or dependencies.
4. Preserve the generated crate/runtime version pin in `crates/codegen/emit-cli/src/templates/project_files.rs`.
5. Treat output JSON, schema JSON, exit codes, auth behavior, request serialization, and the route prelude as compatibility contracts; add focused tests before changing them.

## Testing layers

- Put implementation-level tests beside the runtime module.
- Use `crates/codegen/emit-cli/tests/golden.rs` when generated source must call the runtime differently.
- Use the relevant case in `crates/codegen/emit-cli/tests/compile_gate.rs` when behavior must be proven by compiling or executing a generated CLI.
- Update only the fixture and golden case that exercise the contract.

## Focused validation

```sh
cargo test -p tokyo-cli-runtime <test-name> --locked
cargo check -p tokyo-cli-runtime --locked
cargo test -p tokyo-emit-cli --test golden <test-name> --locked
cargo test -p tokyo-emit-cli --test compile_gate --locked -- --ignored
```

The compile gate is intentionally expensive; run it only when generated/runtime integration requires it. Prefer a runtime test or targeted golden test for local changes.
