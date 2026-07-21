# Tokyo

We think that CLIs are the future of how AI agents will interact with APIs, but (as people that have made CLIs) building a good agent-friendly CLI has some tricky parts to it. So we made Tokyo, which is a route-first Rust framework for CLIs designed for agents. It's like Nextjs but for CLIs.

Filesystem routes work on their own; OpenAPI 3.0/3.1 is an optional source of additional generated commands.

The workspace targets Rust 1.90, edition 2024. The `tokyo` binary generates a
standalone Cargo application that pins `tokyo-cli-runtime` to the same version.

## Create a project

```sh
tokyo init my-cli --name my-cli
cd my-cli
tokyo generate
cargo run -- index
```

Add commands under `src/routes/**`. Each route file exports
`pub fn route() -> Route`, and its path becomes the command path:

```text
src/routes/index.rs           -> my-cli index
src/routes/users/list_all.rs  -> my-cli users list-all
```

`src/middleware.rs` decorates filesystem routes. `src/commands/guidance.rs`
adds project-specific instructions to agent discovery, and
`src/presentation.rs` customizes human help and styling.

Run `tokyo generate` after adding, moving, or deleting routes. During active
development, `tokyo dev` watches the config, OpenAPI snapshot, scenarios, and
source tree; it regenerates when needed, runs `cargo check`, and keeps
`.tokyo/bin/<name>` pointed at the latest successful build.

## Ownership

Tokyo manages `src/tokyo/**`, `src/cli.rs`, `src/main.rs`, and `.tokyo/**`.
Developers own `src/routes/**`, `src/middleware.rs`,
`src/commands/guidance.rs`, `src/presentation.rs`, and the scaffolded Cursor
skills under `.cursor/skills/**`.

Managed files are recorded with SHA-256 hashes in `.tokyo/manifest.json`.
Generation refuses to overwrite a hand-edited managed file and removes only
stale manifest-listed files. Missing managed files are recreated. Scaffolded
skills and other developer-owned starter files are never overwritten.

`src/commands/custom.rs` remains as a legacy compatibility hook; new commands
should use filesystem routes.

## Optional OpenAPI

Vendor an API document explicitly, then generate from the local snapshot:

```sh
tokyo openapi add https://api.example.com/openapi.json
tokyo generate
tokyo openapi check
tokyo openapi sync
tokyo generate
```

`openapi add` accepts an HTTP(S) URL or local path, records the source in
`tokyo.toml`, and writes `openapi/upstream.json` plus `tokyo.lock`.
`generate`, `check`, and `dev` read that vendored snapshot and never fetch the
configured source. Use `openapi check` in CI to detect drift and `openapi sync`
when intentionally accepting an upstream change.

Tokyo imports bundled OpenAPI 3.1 and performs best-effort normalization of
common OpenAPI 3.0 differences. Unsupported constructs fail with contextual
errors instead of being silently omitted.

## Generator commands

- `tokyo init`: create a route-first Cargo project and its Cursor skills.
- `tokyo generate`: emit managed files and scaffold missing developer-owned
  starters.
- `tokyo check`: report generated drift without writing.
- `tokyo dev`: watch, regenerate, check, and maintain the development binary.
- `tokyo diff [--format human|json]`: compare the persisted IR snapshot with
  current input.
- `tokyo openapi add|check|sync`: manage the vendored OpenAPI source.
- `tokyo update-branch`: create a generated-source-only Git branch, optionally
  validate it, push it, and create or update its GitHub pull request.

## Agent interface

Generated CLIs expose:

- `start`: caller-aware JSON orientation and executable next steps.
- `schema`: a compact command index or full detail for one operation.
- `achieve`: goal-oriented create/finalize outcomes inferred from the API.
- `run`: inspectable data-defined scenarios with loops and structured output.
- Resource commands: typed path, query, header, and body arguments.
- `api`: a generic HTTP escape hatch.
- `auth`: credential acquisition, login, identity, logout, and diagnostics.
- `profile` and `env`: persisted connection profiles and named environments.
- `completions`: shell completion generation.
- `reset`: clear the CLI's saved session state.

For scripts and coding agents, invoke commands with `--output json --no-input`.
Successful machine output is written to stdout; failures use one JSON envelope
on stderr containing a stable error code, retryability, and a recovery hint.
Streaming commands emit incrementally, while binary responses are written
byte-for-byte.

## Repository

```text
apps/codegen-cli/                 generator executable
crates/cli-runtime/               shared generated-CLI runtime
crates/codegen/codegen-engine/    orchestration and config
crates/codegen/import-openapi/    OpenAPI 3.0/3.1 importer
crates/codegen/ir/                normalized, serializable API model
crates/codegen/emit-cli/          generated Rust CLI emitter
examples/                         focused OpenAPI fixtures
```

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo test -p tokyo-emit-cli --test compile_gate --locked -- --ignored
python3 scripts/verify-release.py
```

The compile gate builds and exercises every checked-in generated CLI. Release
validation checks crate metadata and packaging for all six workspace packages;
publishing support lives in `scripts/publish-crates.py` and the release
workflow.

Licensed under the MIT License.
