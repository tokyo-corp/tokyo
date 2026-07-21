---
name: tokyo-framework-codegen
description: Guides changes to Tokyo project scaffolding, route discovery, generated-file ownership, manifests, and transactional writes. Applies when editing init/generate/check/dev behavior, managed paths, starter files, route registry generation, or filesystem safety in the Tokyo codegen CLI.
---
# Tokyo framework codegen

## Work from the ownership boundary

- Treat `apps/codegen-cli/src/main.rs` as the filesystem-owning frontend: CLI commands, `scaffold_files`, `install_scaffold`, route discovery, manifest migration, hand-edit checks, stale-file cleanup, staging, and transactional writes live here.
- Treat `crates/codegen/codegen-engine/src/lib.rs` as path-free orchestration. Keep paths, manifests, and filesystem transactions out of this crate.
- Treat `crates/codegen/emit-cli/src/lib.rs` and `crates/codegen/emit-cli/src/templates/` as deterministic source emission from in-memory `tokyo_ir::Api`.

## Preserve ownership

- Managed output includes `.tokyo/**`, `src/tokyo/**`, `src/cli.rs`, and `src/main.rs`; `.tokyo/manifest.json` records managed files and SHA-256 hashes.
- Preserve legacy reads from `.tokyo-manifest.json` and `.tokyo-ir.json` when changing migration behavior.
- Never overwrite or hash `tokyo_emit_cli::UNMANAGED_STARTER_FILES`: `.cursor/skills/**`, `src/commands/{mod,custom,guidance}.rs`, `src/middleware.rs`, and `src/presentation.rs`.
- Preserve `src/routes/**` as user-owned. Generated `src/tokyo/routes.rs` may register and dispatch discovered routes.
- Keep generation fail-closed for hand-edited managed files, remove only stale manifest-listed files, and leave unlisted files untouched.
- Update scaffold and ownership tests whenever a path changes; do not update generated output by hand.

## Change workflow

1. Identify whether the change belongs to initial `tokyo init` scaffolding, emitter output, route registry generation, or manifest/write policy.
2. Make path validation and symlink checks explicit before writes.
3. Keep output ordering deterministic and transactions rollback-safe.
4. Add or adjust focused coverage in `apps/codegen-cli/tests/cli.rs`; use `apps/codegen-cli/tests/openapi.rs` only for vendoring behavior.

## Focused validation

Run only the smallest relevant commands:

```sh
cargo test -p tokyo-cli --test cli <test-name> --locked
cargo test -p tokyo-cli --lib --locked
cargo check -p tokyo-cli --locked
```

Use a test-name filter for ownership, scaffold, route, migration, or transaction changes. Do not run the workspace suite unless explicitly requested.
