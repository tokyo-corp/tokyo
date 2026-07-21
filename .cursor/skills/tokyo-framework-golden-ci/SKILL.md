---
name: tokyo-framework-golden-ci
description: Guides deterministic Tokyo golden regeneration, generated-CLI compile-gate checks, and CI-aligned validation. Applies when fixtures, IR, emitter templates, generated source, runtime integration, or checked-in golden CLI projects change.
---
# Tokyo framework golden and CI

## Understand the pipeline

- Inputs are the focused specifications in `examples/*.yaml`.
- `crates/codegen/emit-cli/tests/golden.rs` imports each fixture and compares deterministic emitter output with `crates/codegen/emit-cli/tests/golden/<fixture>/`.
- Existing `tokyo_emit_cli::UNMANAGED_STARTER_FILES` are intentionally preserved during golden updates.
- `crates/codegen/emit-cli/tests/compile_gate.rs` discovers every golden `Cargo.toml`, checks each generated crate in an isolated target directory, runs `schema`, and executes fixture-specific contracts.
- `.github/workflows/ci.yml` separately enforces formatting, workspace Clippy, workspace tests, and the ignored generated-CLI compile gate on Rust 1.90.0.

## Update goldens deliberately

1. First run the comparison to confirm the expected failing surface:

   ```sh
   cargo test -p tokyo-emit-cli --test golden --locked
   ```

2. Regenerate through the test harness:

   ```sh
   UPDATE_GOLDENS=1 cargo test -p tokyo-emit-cli --test golden --locked
   ```

3. Inspect every changed file under `crates/codegen/emit-cli/tests/golden/`. Reject unrelated churn, nondeterministic ordering, accidental starter-file changes, or stale files no longer emitted.
4. Re-run the comparison without `UPDATE_GOLDENS`.
5. Never hand-edit managed golden output to make assertions pass. Change the fixture, importer, IR, engine, emitter, or template that owns the output.

When adding a fixture, update `FIXTURES` in `golden.rs`, add its matching golden directory, and ensure its generated `Cargo.toml` is discoverable by the compile gate.

## Choose validation by impact

```sh
cargo test -p tokyo-emit-cli --test golden --locked
cargo test -p tokyo-emit-cli --test compile_gate --locked -- --ignored
cargo fmt --all --check
cargo clippy -p tokyo-emit-cli --all-targets --all-features --locked -- -D warnings
```

- Run the compile gate after runtime API changes, generated dependency changes, template changes, or executable contract changes.
- Use package-scoped formatting, tests, or Clippy while iterating.
- Do not run `cargo test --workspace --locked` or other broad tests unless explicitly requested; report which CI jobs remain unrun.
